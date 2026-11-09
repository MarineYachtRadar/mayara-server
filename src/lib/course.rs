//! The own ship's active course, as the upstream Signal K server's Course API
//! publishes it: the route being followed and the points the current leg runs
//! between. Mayara does not navigate; it keeps the last values so a GUI that
//! connects later can draw the route on the PPI straight away.

use serde_json::Value;
use std::sync::{Mutex, OnceLock};

/// The Course API paths mayara relays, in the shape Signal K publishes them:
/// `activeRoute` is `{ href, name, pointIndex, pointTotal, reverse }`,
/// `nextPoint` and `previousPoint` carry a `position`. Each is `null` when
/// there is no course.
pub const COURSE_PATHS: [&str; 3] = [
    "navigation.course.activeRoute",
    "navigation.course.nextPoint",
    "navigation.course.previousPoint",
];

static COURSE: Mutex<[Value; COURSE_PATHS.len()]> =
    Mutex::new([Value::Null, Value::Null, Value::Null]);

fn slot(path: &str) -> Option<usize> {
    COURSE_PATHS.iter().position(|p| *p == path)
}

pub fn is_course_path(path: &str) -> bool {
    slot(path).is_some()
}

/// Record a Course API value. Returns whether it changed, so a repeat of the
/// same value is not passed on to every client again.
pub(crate) fn set(path: &str, value: &Value) -> bool {
    let Some(i) = slot(path) else {
        return false;
    };
    let Ok(mut course) = COURSE.lock() else {
        return false;
    };
    if course[i] == *value {
        return false;
    }
    course[i] = value.clone();
    true
}

/// The paths that hold a course value now, with their values.
pub fn current() -> Vec<(&'static str, Value)> {
    let Ok(course) = COURSE.lock() else {
        return Vec::new();
    };
    COURSE_PATHS
        .iter()
        .zip(course.iter())
        .filter(|(_, value)| !value.is_null())
        .map(|(path, value)| (*path, value.clone()))
        .collect()
}

/// Forget the course, returning the paths that held a value. The upstream that
/// set it has gone, so it can no longer be trusted to still be the course.
pub(crate) fn clear() -> Vec<&'static str> {
    let Ok(mut course) = COURSE.lock() else {
        return Vec::new();
    };
    COURSE_PATHS
        .iter()
        .zip(course.iter_mut())
        .filter(|(_, value)| !value.is_null())
        .map(|(path, value)| {
            *value = Value::Null;
            *path
        })
        .collect()
}

/// Why a route could not be read from the upstream Signal K server.
#[derive(Debug, PartialEq)]
pub enum RouteFetchError {
    /// Not a route id a Signal K server hands out.
    BadId,
    /// No upstream Signal K server with a REST API is known: navigation comes
    /// from NMEA 0183, or from a plain `tcp:` Signal K stream.
    NoUpstream,
    /// The upstream answered with this HTTP status.
    Upstream(u16),
    /// The upstream could not be reached or did not answer with JSON.
    Unreachable(String),
    /// mayara followed another Signal K server while the route was read, so
    /// the answer may not be the route that is now active.
    Switched,
}

/// Route ids are UUIDs on a stock server; a provider may use other names, but
/// never anything that could step out of `resources/routes/`.
fn is_route_id(id: &str) -> bool {
    !id.chars().all(|c| c == '.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Where the upstream serves route `id`, from the REST base its discovery
/// answer gave (`http://host:3000/signalk/v1/api/`).
fn route_url(upstream_base: &str, id: &str) -> Option<reqwest::Url> {
    reqwest::Url::parse(upstream_base)
        .ok()?
        .join(&format!("/signalk/v2/api/resources/routes/{id}"))
        .ok()
}

/// Read route `id` from the upstream Signal K server's Resources API, so a GUI
/// can draw the whole active route and not only the current leg.
pub async fn fetch_route(id: &str, accept_invalid_certs: bool) -> Result<Value, RouteFetchError> {
    if !is_route_id(id) {
        return Err(RouteFetchError::BadId);
    }
    // Taken before the base, so a base stored meanwhile counts as a change.
    let generation = crate::signalk::upstream_generation();
    let base = crate::signalk::get_upstream_http_base().ok_or(RouteFetchError::NoUpstream)?;
    fetch_route_from(&base, id, accept_invalid_certs, || {
        crate::signalk::upstream_generation() == generation
    })
    .await
}

/// Read route `id` from the server at REST base `base`, and give the answer
/// only if `still_followed` says that server is still the one followed once
/// it has answered.
async fn fetch_route_from(
    base: &str,
    id: &str,
    accept_invalid_certs: bool,
    still_followed: impl Fn() -> bool,
) -> Result<Value, RouteFetchError> {
    let url = route_url(base, id).ok_or(RouteFetchError::NoUpstream)?;
    let answer = read_route(url, accept_invalid_certs).await;
    // Even a refusal from a server mayara has left says nothing about the
    // route now active.
    if !still_followed() {
        return Err(RouteFetchError::Switched);
    }
    answer
}

async fn read_route(
    url: reqwest::Url,
    accept_invalid_certs: bool,
) -> Result<Value, RouteFetchError> {
    // A cache between mayara and the Signal K server must not answer with a
    // route from before an edit.
    let mut request = route_client(accept_invalid_certs)?
        .get(url)
        .header(reqwest::header::CACHE_CONTROL, "no-cache");
    if let Some(token) = crate::signalk::get_signalk_token() {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .await
        .map_err(|e| RouteFetchError::Unreachable(e.to_string()))?;
    if !response.status().is_success() {
        return Err(RouteFetchError::Upstream(response.status().as_u16()));
    }
    response
        .json()
        .await
        .map_err(|e| RouteFetchError::Unreachable(e.to_string()))
}

const ROUTE_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// One client per certificate policy, so the connection to the upstream is
/// reused and a request never runs on a client with another policy.
fn route_client(accept_invalid_certs: bool) -> Result<&'static reqwest::Client, RouteFetchError> {
    static CLIENTS: [OnceLock<reqwest::Client>; 2] = [OnceLock::new(), OnceLock::new()];
    let slot = &CLIENTS[usize::from(accept_invalid_certs)];
    if let Some(client) = slot.get() {
        return Ok(client);
    }
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(accept_invalid_certs)
        .timeout(ROUTE_FETCH_TIMEOUT)
        .build()
        .map_err(|e| RouteFetchError::Unreachable(e.to_string()))?;
    Ok(slot.get_or_init(|| client))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A Signal K server that answers one request with `route`, running
    /// `on_request` first. Gives its REST base, and the request it was sent.
    async fn upstream(
        route: &'static str,
        on_request: impl FnOnce() + Send + 'static,
    ) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/signalk/v1/api/", listener.local_addr().unwrap());
        let served = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let n = stream.read(&mut request).await.unwrap();
            on_request();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{route}",
                route.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&request[..n]).into_owned()
        });
        (base, served)
    }

    /// A route answered by the server mayara left while it was being read
    /// is not passed on: the GUI would draw it as the active route of the
    /// server it now follows (#889).
    #[tokio::test]
    async fn a_route_from_a_server_mayara_has_left_is_not_given() {
        let switched = Arc::new(AtomicBool::new(false));
        let switch = switched.clone();
        let (base, _) = upstream(r#"{"name":"old"}"#, move || {
            switch.store(true, Ordering::SeqCst)
        })
        .await;

        let read = fetch_route_from(&base, "r1", false, || !switched.load(Ordering::SeqCst)).await;
        assert_eq!(read, Err(RouteFetchError::Switched));
    }

    /// The route is read past any cache between mayara and the server, so
    /// an edit shows with the next read.
    #[tokio::test]
    async fn a_route_is_read_fresh_from_the_server_still_followed() {
        let (base, served) = upstream(r#"{"name":"r1"}"#, || {}).await;

        let read = fetch_route_from(&base, "r1", false, || true).await;
        assert_eq!(read, Ok(json!({ "name": "r1" })));
        let request = served.await.unwrap().to_ascii_lowercase();
        assert!(
            request.starts_with("get /signalk/v2/api/resources/routes/r1 "),
            "{request}"
        );
        assert!(request.contains("cache-control: no-cache"), "{request}");
    }

    // The course is process-wide state, so the tests that touch it run as one.
    #[test]
    fn course_values_are_kept_reported_and_cleared() {
        clear();
        let next = json!({ "position": { "latitude": -17.7, "longitude": 177.4 } });

        assert!(set("navigation.course.nextPoint", &next));
        assert!(
            !set("navigation.course.nextPoint", &next),
            "a repeat is no change"
        );
        assert!(!set(
            "navigation.course.calcValues.bearingTrue",
            &json!(1.0)
        ));
        assert_eq!(current(), vec![("navigation.course.nextPoint", next)]);

        assert_eq!(clear(), vec!["navigation.course.nextPoint"]);
        assert!(current().is_empty());
        assert!(clear().is_empty(), "nothing left to clear");

        assert!(set(
            "navigation.course.activeRoute",
            &json!({ "href": "/resources/routes/a" })
        ));
        assert!(set("navigation.course.activeRoute", &Value::Null));
        assert!(current().is_empty(), "a null value is no course");
    }

    #[test]
    fn a_route_is_read_from_the_upstream_resources_api() {
        assert_eq!(
            route_url(
                "http://10.0.0.5:3000/signalk/v1/api/",
                "ac3a3b2d-07e8-4f25-92bc-98e7c92f7f1a"
            )
            .unwrap()
            .as_str(),
            "http://10.0.0.5:3000/signalk/v2/api/resources/routes/ac3a3b2d-07e8-4f25-92bc-98e7c92f7f1a"
        );
        assert_eq!(
            route_url("https://boat.local/signalk/v1/api", "r1")
                .unwrap()
                .as_str(),
            "https://boat.local/signalk/v2/api/resources/routes/r1"
        );
    }

    #[test]
    fn only_plain_route_ids_are_asked_for() {
        assert!(is_route_id("ac3a3b2d-07e8-4f25-92bc-98e7c92f7f1a"));
        assert!(is_route_id("harbour_entry"));
        assert!(is_route_id("harbour.entry"));
        assert!(!is_route_id(""));
        assert!(!is_route_id("."));
        assert!(!is_route_id(".."));
        assert!(!is_route_id("..."));
        assert!(!is_route_id("a/../../v1"));
        assert!(!is_route_id("a?x=1"));
    }
}
