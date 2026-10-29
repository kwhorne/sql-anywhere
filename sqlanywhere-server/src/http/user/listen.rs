use crate::broadcaster::BroadcastMsg;
use crate::error::Error;
use crate::metrics::{LISTEN_EVENTS_DROPPED, LISTEN_EVENTS_SENT};
use crate::{
    auth::Authenticated,
    namespace::{NamespaceName, NamespaceStore},
};
use axum::extract::State as AxumState;
use axum::http::Uri;
use axum::response::{
    sse::{Event, Sse},
    IntoResponse, Redirect,
};
use axum_extra::extract::Query;
use futures::{Stream, StreamExt};
use hyper::HeaderMap;
use serde::{Deserialize, Serialize};
use std::boxed::Box;
use std::convert::Infallible;
use std::pin::Pin;
use std::time::Duration;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;

use super::db_factory::namespace_from_headers;
use super::AppState;

const LAGGED_MSG: &str = "some changes were lost";
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(15);
const KEEP_ALIVE_TEXT: &str = "keep-alive";

type SseStream = Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    UNKNOWN,
    DELETE,
    INSERT,
    UPDATE,
}

#[derive(Deserialize)]
pub struct ListenQuery {
    table: String,
    action: Option<Vec<Action>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum AggregatorEvent {
    Error(&'static str),
    #[serde(untagged)]
    Changes(BroadcastMsg),
}

enum ListenResponse {
    SSE(Sse<SseStream>),
    Redirect(Redirect),
}

impl IntoResponse for ListenResponse {
    fn into_response(self) -> axum::response::Response {
        match self {
            ListenResponse::SSE(sse) => sse.into_response(),
            ListenResponse::Redirect(redirect) => redirect.into_response(),
        }
    }
}

pub(super) async fn handle_listen(
    auth: Authenticated,
    AxumState(state): AxumState<AppState>,
    headers: HeaderMap,
    uri: Uri,
    query: Query<ListenQuery>,
) -> crate::Result<impl IntoResponse> {
    let namespace = namespace_from_headers(
        &headers,
        state.disable_default_namespace,
        state.disable_namespaces,
    )?;

    if !auth.is_namespace_authorized(&namespace) {
        return Err(Error::NamespaceDoesntExist(namespace.to_string()));
    }

    if let Some(primary_url) = state.primary_url.as_ref() {
        let url = format!(
            "{}{}",
            primary_url,
            uri.path_and_query().map_or("", |x| x.as_str())
        );
        return Ok(ListenResponse::Redirect(Redirect::temporary(&url)));
    }
    // Writes reach a replica as frames, past the hooks that announce them: a
    // listener here would never hear a commit.
    if state.namespaces.db_kind().is_replica() {
        return Err(Error::NotAPrimary);
    }

    let stream = sse_stream(
        state.namespaces.clone(),
        namespace,
        query.table.clone(),
        query.action.clone(),
    );

    Ok(ListenResponse::SSE(
        Sse::new(stream).keep_alive(
            axum::response::sse::KeepAlive::new()
                .interval(KEEP_ALIVE_INTERVAL)
                .text(KEEP_ALIVE_TEXT),
        ),
    ))
}

fn sse_stream(
    store: NamespaceStore,
    namespace: NamespaceName,
    table: String,
    actions: Option<Vec<Action>>,
) -> SseStream {
    Box::pin(
        listen_stream(store, namespace, table, actions).map(|result| {
            Ok(match result {
                Ok(AggregatorEvent::Error(msg)) => Event::default().event("error").data(msg),
                Ok(AggregatorEvent::Changes(msg)) => {
                    Event::default().event("changes").json_data(msg).unwrap()
                }
                Err(e) => Event::default().event("error").data(e.to_string()),
            })
        }),
    )
}

fn listen_stream(
    store: NamespaceStore,
    namespace: NamespaceName,
    table: String,
    actions: Option<Vec<Action>>,
) -> impl Stream<Item = crate::Result<AggregatorEvent>> {
    // Subscribe now, before the response goes out, not when the body is first
    // polled: a client that has the response must not miss a commit that
    // follows it.
    let mut listening = Listening {
        stream: store.subscribe(namespace.clone(), table.clone()),
        _sub: Subscription::new(store, namespace, table),
    };
    async_stream::try_stream! {
        while let Some(item) = listening.stream.next().await {
            match item {
                Ok(msg) => if filter_actions(&msg, &actions) {
                    LISTEN_EVENTS_SENT.increment(1);
                    yield AggregatorEvent::Changes(msg);
                },
                Err(BroadcastStreamRecvError::Lagged(n)) => {
                    LISTEN_EVENTS_DROPPED.increment(n as u64);
                    yield AggregatorEvent::Error(LAGGED_MSG);
                },
            }
        }
    }
}

fn filter_actions(msg: &BroadcastMsg, actions: &Option<Vec<Action>>) -> bool {
    actions.as_ref().map_or(true, |actions| {
        actions.iter().any(|action| {
            let count = match action {
                Action::DELETE => msg.delete,
                Action::INSERT => msg.insert,
                Action::UPDATE => msg.update,
                Action::UNKNOWN => msg.unknown,
            };
            count > 0
        })
    })
}

/// A subscription and its receiver. Fields drop in order, so the receiver is
/// gone by the time the subscription asks whether anyone still listens.
struct Listening {
    stream: BroadcastStream<BroadcastMsg>,
    _sub: Subscription,
}

struct Subscription {
    store: NamespaceStore,
    namespace: NamespaceName,
    table: String,
}

impl Subscription {
    fn new(store: NamespaceStore, namespace: NamespaceName, table: String) -> Self {
        Self {
            store,
            namespace,
            table,
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.store.unsubscribe(self.namespace.clone(), &self.table);
    }
}
