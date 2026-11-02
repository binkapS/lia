use lia::config::{Config, ProviderKind};
use lia::provider::Provider;
use lia::provider::types::{CompletionRequest, Message};

#[tokio::test]
async fn stream_works() {
    let base = "http://127.0.0.1:8080/v1";
    let cfg: Config = toml::from_str(&format!("provider = \"llama.cpp\"\nbase_url = \"{base}\"")).unwrap();
    let provider = cfg.build_provider().unwrap();
    let req = CompletionRequest { model: "".into(), system: None, messages: vec![Message::user("Say hi")], tools: vec![], max_output_tokens: Some(8), temperature: None, extra: None };
    let mut stream = provider.stream(&req).await.unwrap();
    use futures_util::StreamExt;
    let mut got = 0;
    let res = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while let Some(item) = stream.next().await {
            if let Ok(ev) = item {
                if matches!(ev, lia::provider::types::StreamEvent::Text(_)) {
                    got += 1;
                }
            } else {
                eprintln!("stream err: {:?}", item);
            }
        }
    }).await;
    println!("got text events: {} timeout={}", got, res.is_err());
    assert!(got > 0);
}
