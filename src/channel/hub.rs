//! The channels that are running, by name.
//!
//! A usual reply knows where it is going: it answers a message that came
//! in on that channel, and the adapter that delivered the question takes
//! the answer back. A message the assistant starts by itself has no
//! message to answer, so it is addressed by name — and there has to be
//! one place that knows which channels are up: this hub.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use super::Channel;

/// Every channel the gateway runs, keyed by the channel's name.
#[derive(Clone, Default)]
pub struct ChannelHub {
    channels: Arc<RwLock<HashMap<String, Arc<dyn Channel>>>>,
}

impl ChannelHub {
    /// An empty hub.
    pub fn new() -> ChannelHub {
        ChannelHub::default()
    }

    /// Put a channel in the hub.
    ///
    /// A name that was filled before is replaced, though there is not
    /// usually more than one channel of a kind.
    pub fn add(&self, channel: Arc<dyn Channel>) {
        let name = channel.name().to_string();
        self.channels.write().unwrap().insert(name, channel);
    }

    /// The channel called `name`, if it is running.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Channel>> {
        self.channels.read().unwrap().get(name).cloned()
    }

    /// The names of the channels that are running.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.channels.read().unwrap().keys().cloned().collect();
        names.sort();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{InboundSender, OutboundMessage, OutboundSender};

    /// A channel that answers nobody, only answers to its name.
    struct MockChannel {
        name: &'static str,
    }

    impl MockChannel {
        fn new(name: &'static str) -> Arc<dyn Channel> {
            Arc::new(MockChannel { name })
        }
    }

    #[async_trait::async_trait]
    impl Channel for MockChannel {
        fn name(&self) -> &str {
            self.name
        }

        async fn run(
            &self,
            _inbound: InboundSender,
            _outbound: OutboundSender,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn send(&self, _message: &OutboundMessage) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// A hub remembers what it was given, by name.
    #[test]
    fn a_hub_knows_its_channels_by_name() {
        let hub = ChannelHub::new();
        assert!(hub.get("telegram").is_none());

        hub.add(MockChannel::new("telegram"));
        hub.add(MockChannel::new("discord"));

        assert_eq!(hub.names(), vec!["discord", "telegram"]);
        assert!(hub.get("telegram").is_some());
        assert!(hub.get("slack").is_none());
    }
}
