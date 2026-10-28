# MCP

An MCP server is another program that offers tools. It runs either as a child
process speaking JSON-RPC over its standard streams, or as an HTTP service
speaking JSON-RPC over POST.

```toml
[mcp_servers.notes]
transport = "stdio"           # or "streamable_http", or "sse"
command = "mcp-server-filesystem"
args = ["/home/you/projects"]

[mcp_servers.docs]
transport = "streamable_http"
url = "http://127.0.0.1:8931/mcp"
```

Each server's tools are offered to the model as `<server>__<tool>`, so two
servers offering a `search` tool do not collide. A server that cannot be
reached is reported and skipped: one broken server should not stop the
assistant from working.

### The file server

lia has a file server of its own, so the folders the assistant may reach do
not have to be the folder it was started in. Name them, and lia starts the
server itself — there is nothing to install and nothing else to write down.

```toml
[filesystem]
allow = ["~/projects", "~/notes"]
```

The list is the permission. The server offers `read_file`, `write_file`,
`edit_file`, `list_directory`, `search_files`, `create_directory`,
`move_file` and `get_file_info` inside those folders and nowhere else, and
they reach the model as `files__read_file` and the rest. A path that leaves
the folders is refused, whether it climbs out with `..`, points somewhere
else outright, or arrives through a symbolic link.

A path is relative to one of the folders: each is tried in turn, and the
first that has the file wins. A file that is nowhere yet is made in the
first one.

```toml
[filesystem]
allow = ["~/projects"]
read_only = true          # may read, may not write
```

Read-only is the server's own doing: the writing tools are not offered at
all, so the model is never asked to do something the server would refuse.

The tools that change something say so, and lia asks before they run, the
way it does for its own writing tools. A third-party server that says
nothing keeps the trust it had before.

`mcp_enabled = false` turns the file server off along with the other MCP
servers, and a server of your own named `files` keeps the name, so lia's
own is not started.

`lia mcp-files --allow ~/projects` runs the same server on its own, which
is what lia does when it starts one.
