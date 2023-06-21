use std::{io, num::NonZeroU64};

use async_trait;
use futures::{pin_mut, sink::SinkExt, stream::BoxStream, Sink, Stream, StreamExt};
use tokio::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;

use crate::{
    codecs::Decoder,
    common::ping::PingInterval,
    common::websocket::{ConnectSnafu, WebSocketConnector, WebSocketError},
    config::{SourceConfig, SourceContext},
    sources,
    tls::MaybeTlsSettings,
};

use vector_core::config::{LogNamespace, SourceOutput};
use vector_core::event::{Event, LogEvent};

pub(crate) struct WebSocketSource {
    connector: WebSocketConnector,
    decoder: Decoder,
    ping_interval: Option<NonZeroU64>,
    ping_timeout: Option<NonZeroU64>,
}

impl WebSocketSource {
    pub(crate) fn new(
        config: &super::config::WebSocketConfig,
        connector: WebSocketConnector,
        cx: SourceContext,
    ) -> crate::Result<Self> {
        let log_namespace = cx.log_namespace(config.log_namespace);
        let decoder = config.get_decoding_config(Some(log_namespace)).build();

        Ok(Self {
            connector,
            decoder,
            ping_interval: config.ping_interval,
            ping_timeout: config.ping_timeout,
        })
    }

    async fn create_sink_and_stream(
        &self,
    ) -> (
        impl Sink<Message, Error = WebSocketError>,
        impl Stream<Item = Result<Message, WebSocketError>>,
    ) {
        let ws_stream = self.connector.connect_backoff().await;
        ws_stream.split()
    }

    fn check_received_pong_time(&self, last_pong: Instant) -> Result<(), WebSocketError> {
        if let Some(ping_timeout) = self.ping_timeout {
            if last_pong.elapsed() > Duration::from_secs(ping_timeout.into()) {
                return Err(WebSocketError::Io(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Pong not received in time",
                )));
            }
        }

        Ok(())
    }

    pub(crate) async fn run(&self, cx: SourceContext) -> Result<(), ()> {
        Ok(((), ()))
    }
}
