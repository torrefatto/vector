use std::{io, num::NonZeroU64};

use futures::{pin_mut, sink::SinkExt, Sink, Stream, StreamExt};
use tokio::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::{error::Error as WsError, Message};

use crate::{
    codecs::Decoder,
    common::{
        ping::PingInterval,
        websocket::{is_closed, WebSocketConnector},
    },
    config::SourceContext,
    internal_events::{
        ConnectionOpen, OpenGauge, WsConnectionError, WsConnectionShutdown, WsMessageReceived,
    },
};

// use vector_core::config::{LogNamespace, SourceOutput};
// use vector_core::event::{Event, LogEvent};

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
        impl Sink<Message, Error = WsError>,
        impl Stream<Item = Result<Message, WsError>>,
    ) {
        let ws_stream = self.connector.connect_backoff().await;
        ws_stream.split()
    }

    fn check_received_pong_time(&self, last_pong: Instant) -> Result<(), WsError> {
        if let Some(ping_timeout) = self.ping_timeout {
            if last_pong.elapsed() > Duration::from_secs(ping_timeout.into()) {
                return Err(WsError::Io(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Pong not received in time",
                )));
            }
        }

        Ok(())
    }

    async fn handle_text_message(&self, cx: SourceContext, msg: String) -> () {
        emit!(WsMessageReceived {
            url: self.config.endpoint.clone(),
        });

        cx.out.send(msg).await;

        ()
    }

    pub(crate) async fn run(&self, cx: SourceContext) -> Result<(), ()> {
        const PING: &[u8] = b"PING";
        const PONG: &[u8] = b"PONG";

        let (ws_sink, ws_source) = self.create_sink_and_stream().await;
        pin_mut!(ws_sink);
        pin_mut!(ws_source);

        let _open_token = OpenGauge::new().open(|count| emit!(ConnectionOpen { count }));

        // tokio::time::Interval panics if the period arg is zero. Since the struct members are
        // using NonZeroU64 that is not something we need to account for.
        let mut ping_interval = PingInterval::new(self.ping_interval.map(u64::from));

        if let Err(error) = ws_sink.send(Message::Ping(PING.to_vec())).await {
            emit!(WsConnectionError { error });
            return Err(());
        }
        let mut last_pong = Instant::now();

        loop {
            let result = tokio::select! {
                _ = ping_interval.tick() => {
                    match self.check_received_pong_time(last_pong) {
                        Ok(()) => ws_sink.send(Message::Ping(PING.to_vec())).await.map(|_| ()),
                        Err(e) => Err(e)
                    }
                },

                Some(msg) = ws_source.next() => {
                    // Pongs are sent automatically by tungstenite during reading from the stream.
                    match msg {
                        Ok(Message::Ping(_)) => {
                            if let Err(error) = ws_sink.send(Message::Pong(PONG.to_vec())).await {
                                Err(error)
                            } else {
                                Ok(())
                            }
                        },
                        Ok(Message::Pong(_)) => {
                            last_pong = Instant::now();
                            Ok(())
                        },
                        Ok(Message::Text(msg_txt)) => {
                            Ok(self.handle_text_message(cx, msg_txt).await)
                        },
                        Ok(Message::Binary(_msg_bytes)) => {
                            warn!("Unsupported message type received: binary");
                            Ok(())
                        },
                        Ok(Message::Close(_)) => {
                            info!("Received message: connection closed from server");
                            Err(WsError::ConnectionClosed)
                        },
                        Ok(Message::Frame(_)) => {
                            warn!("Unsupported message type received: binary");
                            Ok(())
                        },
                        Err(e) => Err(e),
                    }
                }
            };

            if let Err(error) = result {
                if is_closed(&error) {
                    emit!(WsConnectionShutdown);
                } else {
                    emit!(WsConnectionError { error });
                }
                return Err(());
            }
        }
    }
}
