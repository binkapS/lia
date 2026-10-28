//! The file server, spoken to over its standard streams.
//!
//! The server is lia's own program, so the test starts lia itself and asks
//! it to be the server. Every answer is a line of JSON, exactly as the MCP
//! client expects to read them.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use serde_json::{Value, json};

use lia::config::{McpServerConfig, McpTransport};
use lia::mcp::{McpClient, McpTool};
use lia::tool::Tool;

/// How long an answer may take before the test gives up.
const PATIENCE: Duration = Duration::from_secs(10);

/// The file server, running as a child process.
struct Server {
    child: Child,
    stdin: ChildStdin,
    answers: Receiver<String>,
}

impl Server {
    /// Start the server over the given folders.
    fn start(folders: &[PathBuf], read_only: bool) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_lia"));
        command.arg("--config");
        // A config that is not there: the server needs no settings, and the
        // test should not read the ones of the machine it runs on.
        command.arg(std::env::temp_dir().join("lia-mcp-files-test.toml"));
        command.arg("mcp-files");
        for folder in folders {
            command.arg("--allow").arg(folder);
        }
        if read_only {
            command.arg("--read-only");
        }

        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the file server starts");

        let stdin = child.stdin.take().expect("an input stream");
        let stdout = child.stdout.take().expect("an output stream");
        let (sender, answers) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });

        Server {
            child,
            stdin,
            answers,
        }
    }

    /// Start the server and shake hands with it.
    fn start_and_greet(folders: &[PathBuf], read_only: bool) -> Self {
        let mut server = Server::start(folders, read_only);
        let answer = server.request("initialize", json!({ "protocolVersion": "2024-11-05" }));
        assert_eq!(answer["result"]["protocolVersion"], "2024-11-05");
        server.notify("notifications/initialized", json!({}));
        server
    }

    /// Send a request and wait for its answer.
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        }));
        self.next()
    }

    /// Send a notification, which is not answered.
    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }));
    }

    /// Run a tool.
    fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
    }

    /// The text of a tool's answer.
    fn text(&self, answer: &Value) -> String {
        answer["result"]["content"]
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|block| block["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }

    /// Whether the tool failed.
    fn failed(&self, answer: &Value) -> bool {
        answer["result"]["isError"] == Value::Bool(true)
    }

    fn send(&mut self, message: Value) {
        let mut line = serde_json::to_string(&message).expect("the message is JSON");
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .expect("the server is listening");
        self.stdin.flush().expect("the line is written");
    }

    /// The next line the server wrote.
    fn next(&mut self) -> Value {
        let line = self
            .answers
            .recv_timeout(PATIENCE)
            .expect("the server answers in time");
        serde_json::from_str(&line).expect("the answer is JSON")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A folder that outlives the test.
fn a_folder() -> PathBuf {
    let dir = tempfile::tempdir().expect("a temporary folder");
    dir.keep()
}

/// A file with something in it.
fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("the parent folder is made");
    }
    std::fs::write(path, content).expect("the file is written");
}

#[test]
fn the_handshake_is_answered() {
    let folder = a_folder();
    let mut server = Server::start(std::slice::from_ref(&folder), false);
    let answer = server.request("initialize", json!({ "protocolVersion": "2024-11-05" }));

    assert_eq!(answer["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(answer["result"]["serverInfo"]["name"], "lia-files");
    let instructions = answer["result"]["instructions"]
        .as_str()
        .expect("instructions");
    assert!(
        instructions.contains(&folder.display().to_string()),
        "the instructions name the folder: {instructions}"
    );
}

#[test]
fn the_tools_are_listed() {
    let folder = a_folder();
    let mut server = Server::start_and_greet(std::slice::from_ref(&folder), false);
    let answer = server.request("tools/list", json!({}));

    let tools = answer["result"]["tools"]
        .as_array()
        .expect("a list of tools")
        .clone();
    let names: Vec<String> = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_string))
        .collect();
    for expected in [
        "read_file",
        "write_file",
        "edit_file",
        "list_directory",
        "search_files",
        "create_directory",
        "move_file",
        "get_file_info",
    ] {
        assert!(
            names.contains(&expected.to_string()),
            "{expected} is offered"
        );
    }

    for tool in &tools {
        assert!(
            tool["description"].as_str().is_some_and(|d| !d.is_empty()),
            "every tool says what it does"
        );
        assert_eq!(tool["inputSchema"]["type"], "object");
    }

    // The tools that change something say so, so that the client knows to
    // ask before they run.
    let write = tools
        .iter()
        .find(|tool| tool["name"] == "write_file")
        .expect("the writing tool");
    assert_eq!(write["annotations"]["readOnlyHint"], false);
    let read = tools
        .iter()
        .find(|tool| tool["name"] == "read_file")
        .expect("the reading tool");
    assert_eq!(read["annotations"]["readOnlyHint"], true);
}

#[test]
fn a_file_is_read() {
    let folder = a_folder();
    write_file(&folder.join("a.txt"), "the words of the file");
    let mut server = Server::start_and_greet(std::slice::from_ref(&folder), false);

    let answer = server.call("read_file", json!({ "path": "a.txt" }));
    assert!(!server.failed(&answer));
    assert!(server.text(&answer).contains("the words of the file"));
}

#[test]
fn a_path_outside_the_folders_is_refused() {
    let folder = a_folder();
    let mut server = Server::start_and_greet(std::slice::from_ref(&folder), false);

    let answer = server.call("read_file", json!({ "path": "/etc/passwd" }));
    assert!(server.failed(&answer));
    assert!(
        server.text(&answer).contains("outside"),
        "the reason is given: {}",
        server.text(&answer)
    );
    assert!(!server.text(&answer).contains("root:"));

    // And one that climbs out with `..`.
    let answer = server.call("read_file", json!({ "path": "../../etc/passwd" }));
    assert!(server.failed(&answer));
    assert!(!server.text(&answer).contains("root:"));
}

#[test]
fn a_link_out_of_the_folders_is_refused() {
    let folder = a_folder();
    let outside = a_folder();
    write_file(&outside.join("secret.txt"), "the secret words");
    std::os::unix::fs::symlink(outside.join("secret.txt"), folder.join("link")).expect("a link");

    let mut server = Server::start_and_greet(std::slice::from_ref(&folder), false);
    let answer = server.call("read_file", json!({ "path": "link" }));
    assert!(server.failed(&answer));
    assert!(!server.text(&answer).contains("the secret words"));
}

#[test]
fn a_dangling_link_cannot_be_written_through() {
    let folder = a_folder();
    let outside = a_folder();
    let target = outside.join("new.txt");
    std::os::unix::fs::symlink(&target, folder.join("dangling")).expect("a link");

    let mut server = Server::start_and_greet(std::slice::from_ref(&folder), false);
    let answer = server.call(
        "write_file",
        json!({ "path": "dangling", "content": "written" }),
    );
    assert!(server.failed(&answer));
    assert!(
        !target.exists(),
        "the file was not written outside the folder"
    );
}

#[test]
fn a_file_is_written() {
    let folder = a_folder();
    let mut server = Server::start_and_greet(std::slice::from_ref(&folder), false);

    let answer = server.call(
        "write_file",
        json!({ "path": "sub/new.txt", "content": "the written words" }),
    );
    assert!(!server.failed(&answer), "{}", server.text(&answer));
    assert_eq!(
        std::fs::read_to_string(folder.join("sub/new.txt")).expect("the file is there"),
        "the written words"
    );

    // And it can be read back.
    let answer = server.call("read_file", json!({ "path": "sub/new.txt" }));
    assert!(server.text(&answer).contains("the written words"));
}

#[test]
fn a_read_only_server_refuses_a_write() {
    let folder = a_folder();
    let mut server = Server::start_and_greet(std::slice::from_ref(&folder), true);

    let answer = server.request("tools/list", json!({}));
    let names: Vec<String> = answer["result"]["tools"]
        .as_array()
        .expect("a list of tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_string))
        .collect();
    assert!(names.contains(&"read_file".to_string()));
    assert!(!names.contains(&"write_file".to_string()));

    let answer = server.call("write_file", json!({ "path": "a.txt", "content": "x" }));
    assert!(server.failed(&answer));
    assert!(server.text(&answer).contains("read-only"));
    assert!(!folder.join("a.txt").exists());
}

#[test]
fn the_second_folder_is_used_when_the_first_does_not_have_the_file() {
    let first = a_folder();
    let second = a_folder();
    write_file(&second.join("b.txt"), "the words of the second folder");

    let mut server = Server::start_and_greet(&[first, second], false);
    let answer = server.call("read_file", json!({ "path": "b.txt" }));
    assert!(!server.failed(&answer), "{}", server.text(&answer));
    assert!(
        server
            .text(&answer)
            .contains("the words of the second folder")
    );
}

#[test]
fn a_new_file_is_made_in_the_first_folder() {
    let first = a_folder();
    let second = a_folder();
    let mut server = Server::start_and_greet(&[first.clone(), second.clone()], false);

    let answer = server.call("write_file", json!({ "path": "new.txt", "content": "x" }));
    assert!(!server.failed(&answer), "{}", server.text(&answer));
    assert!(first.join("new.txt").exists());
    assert!(!second.join("new.txt").exists());
}

#[test]
fn an_unknown_tool_is_refused() {
    let folder = a_folder();
    let mut server = Server::start_and_greet(std::slice::from_ref(&folder), false);

    let answer = server.call("nope", json!({}));
    assert_eq!(answer["error"]["code"], -32602);
}

#[test]
fn an_unknown_method_is_refused() {
    let folder = a_folder();
    let mut server = Server::start_and_greet(std::slice::from_ref(&folder), false);

    let answer = server.request("nope", json!({}));
    assert_eq!(answer["error"]["code"], -32601);
}

#[test]
fn a_notification_gets_no_answer() {
    let folder = a_folder();
    let mut server = Server::start_and_greet(std::slice::from_ref(&folder), false);

    server.notify("notifications/cancelled", json!({}));
    let answer = server.request("tools/list", json!({}));
    assert!(
        answer["result"]["tools"].as_array().is_some(),
        "the next answer is the one that was asked for"
    );
}

#[test]
fn a_file_is_moved() {
    let folder = a_folder();
    write_file(&folder.join("old.txt"), "the words");
    let mut server = Server::start_and_greet(std::slice::from_ref(&folder), false);

    let answer = server.call(
        "move_file",
        json!({ "source": "old.txt", "destination": "new.txt" }),
    );
    assert!(!server.failed(&answer), "{}", server.text(&answer));
    assert!(!folder.join("old.txt").exists());
    assert!(folder.join("new.txt").exists());
}

#[test]
fn a_directory_is_made_and_described() {
    let folder = a_folder();
    let mut server = Server::start_and_greet(std::slice::from_ref(&folder), false);

    let answer = server.call("create_directory", json!({ "path": "a/b/c" }));
    assert!(!server.failed(&answer), "{}", server.text(&answer));
    assert!(folder.join("a/b/c").is_dir());

    let answer = server.call("get_file_info", json!({ "path": "a" }));
    assert!(!server.failed(&answer), "{}", server.text(&answer));
    assert!(server.text(&answer).contains("directory"));
}

#[test]
fn the_files_are_searched() {
    let folder = a_folder();
    write_file(&folder.join("one.txt"), "the needle is here");
    let mut server = Server::start_and_greet(std::slice::from_ref(&folder), false);

    let answer = server.call("search_files", json!({ "content": "needle" }));
    assert!(!server.failed(&answer), "{}", server.text(&answer));
    assert!(server.text(&answer).contains("one.txt"));

    let answer = server.call("search_files", json!({ "pattern": "*.txt" }));
    assert!(server.text(&answer).contains("one.txt"));
}

/// The client and the server, talking to each other.
#[tokio::test]
async fn lia_can_use_its_own_server() {
    let folder = a_folder();
    write_file(&folder.join("a.txt"), "the words of the file");

    let config = McpServerConfig {
        transport: McpTransport::Stdio,
        command: Some(env!("CARGO_BIN_EXE_lia").to_string()),
        args: vec![
            "mcp-files".to_string(),
            "--allow".to_string(),
            folder.to_string_lossy().into_owned(),
        ],
        timeout_secs: 10,
        ..Default::default()
    };

    let client = McpClient::connect("files", &config)
        .await
        .expect("the server answers");

    let tools = client.list_tools().await.expect("the tools are listed");
    assert_eq!(tools.len(), 8);

    let output = client
        .call_tool("read_file", json!({ "path": "a.txt" }))
        .await
        .expect("the file is read");
    assert!(!output.is_error, "{}", output.content);
    assert!(output.content.contains("the words of the file"));

    // The tool that changes something asks before it runs; the one that
    // only reads does not.
    let client = std::sync::Arc::new(tokio::sync::Mutex::new(client));
    let write = tools
        .iter()
        .find(|tool| tool.name == "write_file")
        .expect("the writing tool")
        .clone();
    let read = tools
        .iter()
        .find(|tool| tool.name == "read_file")
        .expect("the reading tool")
        .clone();

    assert!(McpTool::new("files", write, client.clone()).requires_approval());
    assert!(!McpTool::new("files", read, client).requires_approval());
}

/// The server lia starts for itself.
#[test]
fn lia_starts_the_server_itself() {
    let mut config = lia::Config::default();
    config.filesystem.allow = vec![PathBuf::from("/home/you/projects")];
    config.filesystem.read_only = true;

    let servers = lia::cli::mcp_servers(&config);
    let files = servers.get("files").expect("the file server is there");
    assert_eq!(files.transport, McpTransport::Stdio);
    assert!(files.enabled);
    assert!(files.args.contains(&"mcp-files".to_string()));
    assert!(files.args.contains(&"--allow".to_string()));
    assert!(files.args.contains(&"/home/you/projects".to_string()));
    assert!(files.args.contains(&"--read-only".to_string()));
    // It is the running program itself that serves, so there is nothing to
    // install and nothing to find on the PATH.
    let command = files.command.as_deref().expect("the program");
    let running = std::env::current_exe().expect("the running program");
    assert_eq!(command, running.to_string_lossy());
}

#[test]
fn no_folders_means_no_server() {
    let config = lia::Config::default();
    assert!(lia::cli::mcp_servers(&config).is_empty());
}

#[test]
fn a_server_of_the_same_name_keeps_the_name() {
    let mut config = lia::Config::default();
    config.filesystem.allow = vec![PathBuf::from("/home/you/projects")];
    config.mcp_servers.insert(
        "files".to_string(),
        McpServerConfig {
            command: Some("mcp-server-filesystem".to_string()),
            ..Default::default()
        },
    );

    let servers = lia::cli::mcp_servers(&config);
    let files = servers.get("files").expect("the server is there");
    assert_eq!(
        files.command.as_deref(),
        Some("mcp-server-filesystem"),
        "the server that was asked for by name keeps the name"
    );
}
