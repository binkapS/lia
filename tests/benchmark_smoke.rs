use lia::memory::MemoryStore;
use lia::tool::Tool;
use lia::tool::filesystem::{ReadFileTool, Workspace};
use serde_json::json;
use tempfile::tempdir;

#[tokio::test]
async fn benchmark_smoke() {
    let dir = tempdir().unwrap();
    let store = MemoryStore::open(dir.path().join("mem"), dir.path().to_path_buf()).unwrap();
    for i in 0..100 {
        let _ = store.remember(&format!("fact {i}"), lia::memory::Origin::Agent);
    }
    let hits = store.vector_search("fact 50");
    assert!(!hits.is_empty());

    let ws = Workspace::new(dir.path());
    std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
    let tool = ReadFileTool::new(ws);
    let out = tool.run(json!({"path":"a.txt"})).await.unwrap();
    assert!(!out.is_error);
}
