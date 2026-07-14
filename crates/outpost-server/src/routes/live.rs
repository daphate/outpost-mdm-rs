//! `/api/v1/live/stream` — Server-Sent Events feed for the situational maps (Ф1).
//!
//! An operator subscribes to the tenant's live bus; position and marker updates
//! are pushed as SSE events (`position` / `marker`), with `resync` sent when the
//! subscriber lagged and dropped messages (client should refetch the GeoJSON).
//! Requires `AuthUser`, so the connection is authenticated before streaming and
//! is scoped to the caller's `customer_id`.

use crate::auth_extract::AuthUser;
use crate::state::AppState;
use axum::{
    Router,
    extract::State,
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
    routing::get,
};
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/live/stream", get(stream))
}

async fn stream(user: AuthUser, State(state): State<AppState>) -> impl IntoResponse {
    let customer_id = user.customer_id;
    let rx = state.live.subscribe();
    let events = BroadcastStream::new(rx).filter_map(move |res| match res {
        // Only this tenant's events reach the client.
        Ok(ev) if ev.customer_id == customer_id => Some(Ok::<Event, Infallible>(
            Event::default().event(ev.name).data(&*ev.data),
        )),
        Ok(_) => None,
        // Lagged: the receiver fell behind the 1024-slot buffer. Tell the client
        // to refetch rather than trying to replay the gap.
        Err(_lagged) => Some(Ok(Event::default().event("resync").data("{}"))),
    });
    let sse = Sse::new(events).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    );
    // Belt-and-suspenders against proxy buffering (nginx also sets this).
    ([("x-accel-buffering", "no")], sse)
}
