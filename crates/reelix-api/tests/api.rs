//! End-to-end API flows against an in-memory database.

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use reelix_api::{ApiConfig, AppState};
use reelix_core::bus::EventBus;
use reelix_core::{AuthSource, DomainEvent, Permissions};
use reelix_db::secrets::SecretBox;
use reelix_db::{Db, NewUser};
use reelix_jobs::JobSystem;
use serde_json::{Value, json};
use tower::ServiceExt;

const ADMIN_PW: &str = "correct-horse-battery";

struct TestApp {
    router: Router,
    state: AppState,
    _dir: tempfile::TempDir,
}

async fn app() -> TestApp {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::in_memory().await.unwrap();
    let events = EventBus::default();
    let mut builder =
        JobSystem::builder(db.clone(), events.clone()).handler(reelix_jobs::builtin::Housekeeping);
    for t in reelix_jobs::builtin::default_tasks() {
        db.tasks()
            .ensure(
                t.kind,
                t.name,
                t.interval.as_secs() as i64,
                reelix_db::now_ms() + 3_600_000,
            )
            .await
            .unwrap();
        builder = builder.task(t);
    }
    let system = builder.build();
    let config = ApiConfig {
        url_base: String::new(),
        data_dir: dir.path().to_path_buf(),
        database: "sqlite".into(),
        start_time: chrono::Utc::now(),
    };
    let state = AppState::new(db, events, system.queue(), SecretBox::from_key([7u8; 32]), config);
    TestApp {
        router: reelix_api::router(state.clone()),
        state,
        _dir: dir,
    }
}

struct Resp {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

impl Resp {
    /// `name=value` of the Set-Cookie header.
    fn cookie(&self) -> String {
        let sc = self
            .headers
            .get(header::SET_COOKIE)
            .expect("set-cookie")
            .to_str()
            .unwrap();
        sc.split(';').next().unwrap().to_owned()
    }
}

#[derive(Default)]
struct Req<'a> {
    cookie: Option<&'a str>,
    api_key: Option<&'a str>,
    csrf: bool,
    body: Option<Value>,
    headers: Vec<(&'static str, &'a str)>,
    /// TCP peer address (none, like a proxy-less in-process call, by default).
    peer: Option<&'a str>,
}

async fn call(app: &TestApp, method: Method, uri: &str, r: Req<'_>) -> Resp {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(c) = r.cookie {
        b = b.header(header::COOKIE, c);
    }
    if let Some(k) = r.api_key {
        b = b.header("x-api-key", k);
    }
    if r.csrf {
        b = b.header("x-reelix-request", "1");
    }
    for (k, v) in r.headers {
        b = b.header(k, v);
    }
    if let Some(peer) = r.peer {
        let addr: std::net::SocketAddr = peer.parse().unwrap();
        b = b.extension(axum::extract::ConnectInfo(addr));
    }
    let req = match r.body {
        Some(v) => b
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let res = app.router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    Resp {
        status,
        headers,
        body,
    }
}

fn json_body(v: Value) -> Req<'static> {
    Req {
        body: Some(v),
        ..Default::default()
    }
}

/// Runs setup and returns the admin's cookie.
async fn setup_admin(app: &TestApp) -> String {
    let r = call(
        app,
        Method::POST,
        "/api/v1/setup",
        json_body(json!({"username": "admin", "password": ADMIN_PW})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    r.cookie()
}

async fn login(app: &TestApp, username: &str, password: &str, remember: bool) -> Resp {
    call(
        app,
        Method::POST,
        "/api/v1/auth/login",
        json_body(json!({"username": username, "password": password, "remember_me": remember})),
    )
    .await
}

async fn create_regular_user(app: &TestApp) {
    let hash = reelix_api::auth::hash_password("user-password").unwrap();
    app.state
        .db()
        .users()
        .create(NewUser {
            username: "bob".into(),
            email: None,
            display_name: Some("Bob".into()),
            password_hash: Some(hash),
            auth_source: AuthSource::Local,
            permissions: Permissions::ROLE_USER,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn setup_then_me_and_second_setup_conflicts() {
    let app = app().await;
    let mut events = app.state.events().subscribe();

    let r = call(&app, Method::GET, "/api/v1/setup", Req::default()).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["needs_setup"], true);

    let bad = call(
        &app,
        Method::POST,
        "/api/v1/setup",
        json_body(json!({"username": "bad name", "password": ADMIN_PW})),
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    assert_eq!(bad.body["error"], "validation");
    let short = call(
        &app,
        Method::POST,
        "/api/v1/setup",
        json_body(json!({"username": "admin", "password": "short"})),
    )
    .await;
    assert_eq!(short.body["error"], "validation");

    let r = call(
        &app,
        Method::POST,
        "/api/v1/setup",
        json_body(json!({"username": "admin", "password": ADMIN_PW})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(r.body["username"], "admin");
    let set_cookie = r.headers[header::SET_COOKIE].to_str().unwrap().to_owned();
    assert!(
        set_cookie.contains("HttpOnly")
            && set_cookie.contains("SameSite=Lax")
            && set_cookie.contains("Path=/")
    );
    assert!(!set_cookie.contains("Secure"));
    let cookie = r.cookie();

    let me = call(
        &app,
        Method::GET,
        "/api/v1/auth/me",
        Req {
            cookie: Some(&cookie),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.body["username"], "admin");
    assert_eq!(me.body["permissions"], Permissions::ROLE_ADMIN.bits());

    let names: Vec<_> = [events.try_recv().unwrap(), events.try_recv().unwrap()]
        .iter()
        .map(|e| e.name())
        .collect();
    assert!(names.contains(&"setup.completed") && names.contains(&"user.created"));

    let r = call(&app, Method::GET, "/api/v1/setup", Req::default()).await;
    assert_eq!(r.body["needs_setup"], false);
    let again = call(
        &app,
        Method::POST,
        "/api/v1/setup",
        json_body(json!({"username": "other", "password": ADMIN_PW})),
    )
    .await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(again.body["error"], "conflict");
}

#[tokio::test]
async fn unauthenticated_requests_get_401_json() {
    let app = app().await;
    for uri in [
        "/api/v1/auth/me",
        "/api/v1/system/status",
        "/api/v1/system/tasks",
        "/api/v1/ws",
    ] {
        let r = call(&app, Method::GET, uri, Req::default()).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(r.body["error"], "unauthorized");
    }
    let r = call(
        &app,
        Method::GET,
        "/api/v1/auth/me",
        Req {
            cookie: Some("reelix_session=bogus"),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

/// Rebuilds the app's state (fresh limiter) with `f` applied before the router is built.
fn reconfigure(app: &mut TestApp, f: impl FnOnce(AppState) -> AppState) {
    let state = AppState::new(
        app.state.db().clone(),
        app.state.events().clone(),
        app.state.jobs().clone(),
        app.state.secrets().clone(),
        app.state.config().clone(),
    );
    app.state = f(state);
    app.router = reelix_api::router(app.state.clone());
}

#[tokio::test]
async fn setup_requires_the_token_unless_local() {
    let mut app = app().await;
    reconfigure(&mut app, |s| s.with_setup_token("tok-123"));
    let setup = |token: Option<&str>, peer: Option<&'static str>, headers| {
        let mut body = json!({"username": "admin", "password": ADMIN_PW});
        if let Some(t) = token {
            body["setup_token"] = json!(t);
        }
        Req {
            body: Some(body),
            peer,
            headers,
            ..Default::default()
        }
    };

    let r = call(&app, Method::GET, "/api/v1/setup", Req::default()).await;
    assert_eq!(r.body["setup_token_required"], true);
    let r = call(
        &app,
        Method::GET,
        "/api/v1/setup",
        Req {
            peer: Some("127.0.0.1:5000"),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(r.body["setup_token_required"], false);

    for (token, peer, headers) in [
        (None, Some("192.0.2.1:5000"), vec![]),
        (Some("wrong"), None, vec![]),
        // A reverse proxy on the same host doesn't make its clients local.
        (
            None,
            Some("127.0.0.1:5000"),
            vec![("x-forwarded-for", "192.0.2.1")],
        ),
    ] {
        let r = call(&app, Method::POST, "/api/v1/setup", setup(token, peer, headers)).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{:?}", r.body);
    }
    let r = call(
        &app,
        Method::POST,
        "/api/v1/setup",
        setup(Some("tok-123"), None, vec![]),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
}

#[tokio::test]
async fn setup_restore_needs_the_token_and_no_accounts() {
    let mut app = app().await;
    reconfigure(&mut app, |s| s.with_setup_token("tok-123"));
    // Reelix's wizards send the CSRF header; a page on another site cannot.
    let restore = |token: Option<&'static str>, peer: Option<&'static str>| Req {
        peer,
        headers: token
            .map(|t| vec![("x-reelix-setup-token", t)])
            .unwrap_or_default()
            .into_iter()
            .chain([("x-reelix-request", "1")])
            .collect(),
        ..Default::default()
    };
    for (token, peer) in [
        (None, Some("192.0.2.1:5000")),
        (Some("wrong"), Some("192.0.2.1:5000")),
    ] {
        let r = call(&app, Method::POST, "/api/v1/setup/restore", restore(token, peer)).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{:?}", r.body);
    }
    // A cross-site post from a browser on the server itself: no code is needed there, but the CSRF
    // header is, and a page on another site cannot send it without the browser asking first.
    let cross_site = Req {
        peer: Some("127.0.0.1:5000"),
        ..Default::default()
    };
    let r = call(&app, Method::POST, "/api/v1/setup/restore", cross_site).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{:?}", r.body);
    // Past the checks, the upload reaches the backup service (not registered in these tests).
    for (token, peer) in [
        (Some("tok-123"), Some("192.0.2.1:5000")),
        (None, Some("127.0.0.1:5000")),
    ] {
        let r = call(&app, Method::POST, "/api/v1/setup/restore", restore(token, peer)).await;
        assert!(
            r.status != StatusCode::FORBIDDEN && r.status != StatusCode::CONFLICT,
            "{:?}",
            r.status
        );
    }
    let r = call(
        &app,
        Method::POST,
        "/api/v1/setup",
        json_body(json!({"username": "admin", "password": ADMIN_PW, "setup_token": "tok-123"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    let r = call(
        &app,
        Method::POST,
        "/api/v1/setup/restore",
        restore(Some("tok-123"), Some("127.0.0.1:5000")),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{:?}", r.body);
}

#[tokio::test]
async fn setup_from_the_server_needs_a_name_for_this_computer() {
    // DNS rebinding: a page on another site points its own name at 127.0.0.1 and posts from the
    // browser on the server. The request comes from loopback, but its Host is the other site's
    // name, so the set-up code is still needed.
    let mut app = app().await;
    reconfigure(&mut app, |s| s.with_setup_token("tok-123"));
    let rebound = |token: Option<&'static str>, host: &'static str| Req {
        peer: Some("127.0.0.1:5000"),
        headers: token
            .map(|t| vec![("x-reelix-setup-token", t)])
            .unwrap_or_default()
            .into_iter()
            .chain([("x-reelix-request", "1"), ("host", host)])
            .collect(),
        ..Default::default()
    };
    let r = call(
        &app,
        Method::POST,
        "/api/v1/setup/restore",
        rebound(None, "attacker.example:7700"),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{:?}", r.body);
    assert!(r.body.to_string().contains("localhost"), "{:?}", r.body);
    let r = call(
        &app,
        Method::GET,
        "/api/v1/setup",
        rebound(None, "attacker.example:7700"),
    )
    .await;
    assert_eq!(r.body["setup_token_required"], true);
    // With the code, or with a name for this computer, it goes past the checks.
    for (token, host) in [
        (Some("tok-123"), "attacker.example:7700"),
        (None, "localhost:7700"),
        (None, "127.0.0.1:7700"),
        (None, "[::1]:7700"),
    ] {
        let r = call(&app, Method::POST, "/api/v1/setup/restore", rebound(token, host)).await;
        assert!(
            r.status != StatusCode::FORBIDDEN && r.status != StatusCode::CONFLICT,
            "{host}: {:?}",
            r.status
        );
    }
    let r = call(
        &app,
        Method::POST,
        "/api/v1/setup",
        Req {
            body: Some(json!({"username": "admin", "password": ADMIN_PW})),
            peer: Some("127.0.0.1:5000"),
            headers: vec![("host", "attacker.example")],
            ..Default::default()
        },
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{:?}", r.body);
    let r = call(
        &app,
        Method::POST,
        "/api/v1/setup",
        Req {
            body: Some(json!({"username": "admin", "password": ADMIN_PW})),
            peer: Some("127.0.0.1:5000"),
            headers: vec![("host", "localhost:7700")],
            ..Default::default()
        },
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
}

#[tokio::test]
async fn setup_from_the_server_itself_needs_no_token() {
    let mut app = app().await;
    reconfigure(&mut app, |s| s.with_setup_token("tok-123"));
    let r = call(
        &app,
        Method::POST,
        "/api/v1/setup",
        Req {
            body: Some(json!({"username": "admin", "password": ADMIN_PW})),
            peer: Some("[::1]:5000"),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
}

#[tokio::test]
async fn login_ok_bad_and_logout() {
    let app = app().await;
    setup_admin(&app).await;

    let bad = login(&app, "admin", "wrong-password", false).await;
    assert_eq!(bad.status, StatusCode::UNAUTHORIZED);
    assert_eq!(bad.body["error"], "unauthorized");
    let unknown = login(&app, "nobody", "whatever-password", false).await;
    assert_eq!(unknown.status, StatusCode::UNAUTHORIZED);

    let ok = login(&app, "ADMIN", ADMIN_PW, false).await;
    assert_eq!(ok.status, StatusCode::OK);
    assert_eq!(ok.body["username"], "admin");
    assert!(
        !ok.headers[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("Max-Age")
    );

    let remembered = login(&app, "admin", ADMIN_PW, true).await;
    assert!(
        remembered.headers[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("Max-Age=2592000")
    );

    let cookie = ok.cookie();
    let no_csrf = call(
        &app,
        Method::POST,
        "/api/v1/auth/logout",
        Req {
            cookie: Some(&cookie),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(no_csrf.status, StatusCode::FORBIDDEN);
    assert_eq!(no_csrf.body["error"], "csrf");

    let out = call(
        &app,
        Method::POST,
        "/api/v1/auth/logout",
        Req {
            cookie: Some(&cookie),
            csrf: true,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(out.status, StatusCode::NO_CONTENT);
    assert!(
        out.headers[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    let me = call(
        &app,
        Method::GET,
        "/api/v1/auth/me",
        Req {
            cookie: Some(&cookie),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(me.status, StatusCode::UNAUTHORIZED);

    // The other session is unaffected.
    let me = call(
        &app,
        Method::GET,
        "/api/v1/auth/me",
        Req {
            cookie: Some(&remembered.cookie()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(me.status, StatusCode::OK);
}

#[tokio::test]
async fn secure_cookie_behind_https_proxy() {
    let app = app().await;
    setup_admin(&app).await;
    let r = call(
        &app,
        Method::POST,
        "/api/v1/auth/login",
        Req {
            body: Some(json!({"username": "admin", "password": ADMIN_PW})),
            headers: vec![("x-forwarded-proto", "https")],
            ..Default::default()
        },
    )
    .await;
    assert!(
        r.headers[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("; Secure")
    );
}

#[tokio::test]
async fn login_is_rate_limited() {
    let app = app().await;
    setup_admin(&app).await;
    for _ in 0..5 {
        assert_eq!(
            login(&app, "admin", "wrong-password", false).await.status,
            StatusCode::UNAUTHORIZED
        );
    }
    let r = login(&app, "admin", ADMIN_PW, false).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.body["error"], "rate_limited");
    let retry: u64 = r.headers[header::RETRY_AFTER].to_str().unwrap().parse().unwrap();
    assert!((1..=30).contains(&retry));
    // Other usernames are tracked separately.
    assert_eq!(
        login(&app, "someone", "wrong-password", false).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn parallel_guesses_cannot_outrun_the_limiter() {
    let app = app().await;
    setup_admin(&app).await;
    let guesses = (0..12).map(|_| login(&app, "admin", "wrong-password", false));
    let statuses: Vec<_> = futures::future::join_all(guesses)
        .await
        .into_iter()
        .map(|r| r.status)
        .collect();
    let tried = statuses
        .iter()
        .filter(|s| **s == StatusCode::UNAUTHORIZED)
        .count();
    assert!(tried <= 5, "{statuses:?}");
    assert!(statuses.contains(&StatusCode::TOO_MANY_REQUESTS), "{statuses:?}");
}

#[tokio::test]
async fn rate_limit_uses_forwarded_address_only_from_trusted_proxies() {
    let mut app = app().await;
    reconfigure(&mut app, |s| {
        s.with_security(reelix_api::SecurityConfig {
            trusted_proxies: vec!["10.0.0.0/8".parse().unwrap()],
            ..Default::default()
        })
    });
    setup_admin(&app).await;
    let attempt = |peer: &'static str, xff: &'static str| {
        call(
            &app,
            Method::POST,
            "/api/v1/auth/login",
            Req {
                body: Some(json!({"username": "admin", "password": "wrong-password"})),
                peer: Some(peer),
                headers: vec![("x-forwarded-for", xff)],
                ..Default::default()
            },
        )
    };
    // Through the trusted proxy each client is counted on its own…
    for _ in 0..5 {
        assert_eq!(
            attempt("10.0.0.1:1", "192.0.2.1").await.status,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        attempt("10.0.0.1:1", "192.0.2.1").await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        attempt("10.0.0.1:1", "192.0.2.2").await.status,
        StatusCode::UNAUTHORIZED
    );
    // …while an untrusted peer can't dodge its lock by inventing X-Forwarded-For values.
    for i in 0..5 {
        let xff = ["1.1.1.1", "2.2.2.2", "3.3.3.3", "4.4.4.4", "5.5.5.5"][i];
        assert_eq!(
            attempt("198.51.100.1:1", xff).await.status,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        attempt("198.51.100.1:1", "6.6.6.6").await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn secure_cookies_setting() {
    let mut app = app().await;
    setup_admin(&app).await;
    for (mode, proto, secure) in [
        (reelix_api::SecureCookies::Always, "http", true),
        (reelix_api::SecureCookies::Never, "https", false),
    ] {
        reconfigure(&mut app, |s| {
            s.with_security(reelix_api::SecurityConfig {
                secure_cookies: mode,
                ..Default::default()
            })
        });
        let r = call(
            &app,
            Method::POST,
            "/api/v1/auth/login",
            Req {
                body: Some(json!({"username": "admin", "password": ADMIN_PW})),
                headers: vec![("x-forwarded-proto", proto)],
                ..Default::default()
            },
        )
        .await;
        let cookie = r.headers[header::SET_COOKIE].to_str().unwrap().to_owned();
        assert_eq!(cookie.contains("; Secure"), secure, "{mode:?}: {cookie}");
    }
}

#[tokio::test]
async fn csrf_header_required_for_cookie_writes() {
    let app = app().await;
    let cookie = setup_admin(&app).await;
    let body = json!({"name": "test"});
    let r = call(
        &app,
        Method::POST,
        "/api/v1/auth/apikeys",
        Req {
            cookie: Some(&cookie),
            body: Some(body.clone()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.body["error"], "csrf");
    let r = call(
        &app,
        Method::POST,
        "/api/v1/auth/apikeys",
        Req {
            cookie: Some(&cookie),
            body: Some(body),
            csrf: true,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
}

#[tokio::test]
async fn api_key_create_use_delete() {
    let app = app().await;
    let cookie = setup_admin(&app).await;
    let r = call(
        &app,
        Method::POST,
        "/api/v1/auth/apikeys",
        Req {
            cookie: Some(&cookie),
            csrf: true,
            body: Some(json!({"name": "  Sonarr  "})),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let key = r.body["key"].as_str().unwrap().to_owned();
    let id = r.body["api_key"]["id"].as_i64().unwrap();
    assert!(key.starts_with("rlx_"));
    assert_eq!(r.body["api_key"]["name"], "Sonarr");
    assert_eq!(r.body["api_key"]["prefix"], &key[4..12]);

    let empty = call(
        &app,
        Method::POST,
        "/api/v1/auth/apikeys",
        Req {
            cookie: Some(&cookie),
            csrf: true,
            body: Some(json!({"name": " "})),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST);
    let malformed = call(
        &app,
        Method::POST,
        "/api/v1/auth/apikeys",
        Req {
            cookie: Some(&cookie),
            csrf: true,
            body: Some(json!({"nope": 1})),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(malformed.body["error"], "validation");

    // Header and query auth both work; API-key writes need no CSRF header.
    let me = call(
        &app,
        Method::GET,
        "/api/v1/auth/me",
        Req {
            api_key: Some(&key),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.body["username"], "admin");
    let me = call(
        &app,
        Method::GET,
        &format!("/api/v1/auth/me?apikey={key}"),
        Req::default(),
    )
    .await;
    assert_eq!(me.status, StatusCode::OK);
    let list = call(
        &app,
        Method::GET,
        "/api/v1/auth/apikeys",
        Req {
            api_key: Some(&key),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(list.body.as_array().unwrap().len(), 1);
    assert!(list.body[0].get("key").is_none());

    let del = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/auth/apikeys/{id}"),
        Req {
            api_key: Some(&key),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(del.status, StatusCode::NO_CONTENT);
    let again = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/auth/apikeys/{id}"),
        Req {
            cookie: Some(&cookie),
            csrf: true,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
    assert_eq!(again.body["error"], "not_found");
    let me = call(
        &app,
        Method::GET,
        "/api/v1/auth/me",
        Req {
            api_key: Some(&key),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(me.status, StatusCode::UNAUTHORIZED);
    let bad_id = call(
        &app,
        Method::DELETE,
        "/api/v1/auth/apikeys/abc",
        Req {
            cookie: Some(&cookie),
            csrf: true,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(bad_id.body["error"], "validation");
}

#[tokio::test]
async fn admin_endpoints_forbidden_for_regular_users() {
    let app = app().await;
    let admin = setup_admin(&app).await;
    create_regular_user(&app).await;
    let bob = login(&app, "bob", "user-password", false).await;
    assert_eq!(bob.status, StatusCode::OK);
    let bob = bob.cookie();

    for uri in ["/api/v1/system/tasks", "/api/v1/system/jobs"] {
        let r = call(
            &app,
            Method::GET,
            uri,
            Req {
                cookie: Some(&bob),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{uri}");
        assert_eq!(r.body["error"], "forbidden");
    }
    let r = call(
        &app,
        Method::POST,
        "/api/v1/system/tasks/housekeeping/run",
        Req {
            cookie: Some(&bob),
            csrf: true,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // Regular users may still read status.
    let r = call(
        &app,
        Method::GET,
        "/api/v1/system/status",
        Req {
            cookie: Some(&bob),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["database"], "sqlite");
    // …but not where the server keeps its data.
    assert!(r.body.get("data_dir").is_none(), "{:?}", r.body);
    let r = call(
        &app,
        Method::GET,
        "/api/v1/system/status",
        Req {
            cookie: Some(&admin),
            ..Default::default()
        },
    )
    .await;
    assert!(r.body["data_dir"].is_string(), "{:?}", r.body);

    let tasks = call(
        &app,
        Method::GET,
        "/api/v1/system/tasks",
        Req {
            cookie: Some(&admin),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(tasks.status, StatusCode::OK);
    assert!(
        tasks
            .body
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["kind"] == "housekeeping")
    );
}

#[tokio::test]
async fn run_task_and_list_jobs() {
    let app = app().await;
    let admin = setup_admin(&app).await;
    let r = call(
        &app,
        Method::POST,
        "/api/v1/system/tasks/housekeeping/run",
        Req {
            cookie: Some(&admin),
            csrf: true,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{:?}", r.body);
    assert!(r.body["job_id"].is_i64());
    let dup = call(
        &app,
        Method::POST,
        "/api/v1/system/tasks/housekeeping/run",
        Req {
            cookie: Some(&admin),
            csrf: true,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(dup.status, StatusCode::ACCEPTED);
    assert!(dup.body["job_id"].is_null());

    let r = call(
        &app,
        Method::POST,
        "/api/v1/system/tasks/nope/run",
        Req {
            cookie: Some(&admin),
            csrf: true,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let tasks = call(
        &app,
        Method::GET,
        "/api/v1/system/tasks",
        Req {
            cookie: Some(&admin),
            ..Default::default()
        },
    )
    .await;
    let hk = tasks
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["kind"] == "housekeeping")
        .unwrap()
        .clone();
    assert_eq!(hk["running"], true);

    let jobs = call(
        &app,
        Method::GET,
        "/api/v1/system/jobs?limit=1000",
        Req {
            cookie: Some(&admin),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(jobs.status, StatusCode::OK);
    let jobs = jobs.body.as_array().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["kind"], "housekeeping");
    assert_eq!(jobs[0]["status"], "queued");
    let bad = call(
        &app,
        Method::GET,
        "/api/v1/system/jobs?limit=abc",
        Req {
            cookie: Some(&admin),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(bad.body["error"], "validation");
}

#[tokio::test]
async fn health_public_vs_authenticated() {
    let app = app().await;
    let admin = setup_admin(&app).await;
    let public = call(&app, Method::GET, "/api/v1/system/health", Req::default()).await;
    assert_eq!(public.status, StatusCode::OK);
    assert_eq!(public.body["status"], "ok");
    assert_eq!(public.body["checks"].as_array().unwrap().len(), 0);

    let authed = call(
        &app,
        Method::GET,
        "/api/v1/system/health",
        Req {
            cookie: Some(&admin),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(authed.status, StatusCode::OK);
    let sources: Vec<_> = authed.body["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["source"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(sources, ["database", "data_dir"]);
}

#[tokio::test]
async fn health_reports_unwritable_data_dir_as_503() {
    let mut app = app().await;
    let missing = app._dir.path().join("does-not-exist");
    let db = app.state.db().clone();
    let events = app.state.events().clone();
    let queue = app.state.jobs().clone();
    let cfg = ApiConfig {
        data_dir: missing,
        ..app.state.config().clone()
    };
    app.state = AppState::new(db, events, queue, app.state.secrets().clone(), cfg);
    app.router = reelix_api::router(app.state.clone());
    let r = call(&app, Method::GET, "/api/v1/system/health", Req::default()).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r.body["status"], "error");
}

#[tokio::test]
async fn openapi_and_docs_are_served() {
    let app = app().await;
    let r = call(&app, Method::GET, "/api/v1/openapi.json", Req::default()).await;
    assert_eq!(r.status, StatusCode::OK);
    let doc = r.body;
    assert!(doc["openapi"].as_str().unwrap().starts_with("3."));
    assert_eq!(doc["info"]["title"], "Reelix API");
    assert_eq!(doc["info"]["version"], reelix_core::VERSION);
    for p in [
        "/api/v1/setup",
        "/api/v1/auth/login",
        "/api/v1/auth/logout",
        "/api/v1/auth/me",
        "/api/v1/auth/apikeys",
        "/api/v1/auth/apikeys/{id}",
        "/api/v1/system/status",
        "/api/v1/system/health",
        "/api/v1/system/tasks",
        "/api/v1/system/tasks/{kind}/run",
        "/api/v1/system/jobs",
        "/api/v1/ws",
    ] {
        assert!(doc["paths"].get(p).is_some(), "missing path {p}");
    }
    let schemes = &doc["components"]["securitySchemes"];
    assert_eq!(schemes["api_key"]["in"], "header");
    assert_eq!(schemes["session"]["in"], "cookie");
    // Round-trips through utoipa's own model.
    let _: utoipa::openapi::OpenApi = serde_json::from_value(doc).unwrap();

    let docs = call(&app, Method::GET, "/api/docs", Req::default()).await;
    assert_eq!(docs.status, StatusCode::OK);
    assert!(docs.body.as_str().unwrap().to_lowercase().contains("<html"));
}

#[tokio::test]
async fn websocket_forwards_visible_events() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let app = app().await;
    let admin = setup_admin(&app).await;
    create_regular_user(&app).await;
    let bob = login(&app, "bob", "user-password", false).await.cookie();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app.router.clone();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    async fn handshake(
        addr: std::net::SocketAddr,
        cookie: &str,
        origin: &str,
    ) -> (String, tokio::net::TcpStream) {
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        let req = format!(
            "GET /api/v1/ws HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nCookie: {cookie}\r\n\
             Origin: {origin}\r\n\r\n"
        );
        s.write_all(req.as_bytes()).await.unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            s.read_exact(&mut byte).await.unwrap();
            head.push(byte[0]);
        }
        (String::from_utf8(head).unwrap(), s)
    }

    async fn connect(addr: std::net::SocketAddr, cookie: &str) -> tokio::net::TcpStream {
        let (head, s) = handshake(addr, cookie, &format!("http://{addr}")).await;
        assert!(head.starts_with("HTTP/1.1 101"), "{head}");
        s
    }

    // Cookie logins from another site are refused (cross-site WebSocket hijacking).
    for origin in ["https://evil.example", "null", "http://127.0.0.1:1"] {
        let (head, _) = handshake(addr, &admin, origin).await;
        assert!(head.starts_with("HTTP/1.1 403"), "{origin}: {head}");
    }

    async fn read_text(s: &mut tokio::net::TcpStream) -> Value {
        let mut h = [0u8; 2];
        tokio::time::timeout(std::time::Duration::from_secs(5), s.read_exact(&mut h))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(h[0], 0x81, "expected a final text frame");
        let len = match h[1] & 0x7f {
            126 => {
                let mut l = [0u8; 2];
                s.read_exact(&mut l).await.unwrap();
                u16::from_be_bytes(l) as usize
            }
            127 => panic!("frame too large"),
            n => n as usize,
        };
        let mut payload = vec![0u8; len];
        s.read_exact(&mut payload).await.unwrap();
        serde_json::from_slice(&payload).unwrap()
    }

    let mut admin_ws = connect(addr, &admin).await;
    let mut bob_ws = connect(addr, &bob).await;

    let bus = app.state.events();
    bus.publish(DomainEvent::TaskUpdated {
        kind: "housekeeping".into(),
    }); // admin only
    bus.publish(DomainEvent::SetupCompleted { user_id: 1 }); // everyone

    let first = read_text(&mut admin_ws).await;
    assert_eq!(first["type"], "task.updated");
    assert_eq!(first["data"]["kind"], "housekeeping");
    assert_eq!(read_text(&mut admin_ws).await["type"], "setup.completed");
    // Bob never sees the admin-only event.
    assert_eq!(read_text(&mut bob_ws).await["type"], "setup.completed");
}

#[test]
fn audience_filtering() {
    use reelix_core::User;
    let user = |id, permissions| User {
        id,
        username: "u".into(),
        email: None,
        display_name: None,
        auth_source: AuthSource::Local,
        permissions,
        created_at: chrono::Utc::now(),
    };
    let admin = user(1, Permissions::ROLE_ADMIN);
    let bob = user(2, Permissions::ROLE_USER);
    let admin_ev = DomainEvent::JobQueued {
        id: 1,
        kind: "x".into(),
    };
    let all_ev = DomainEvent::SetupCompleted { user_id: 1 };
    assert!(reelix_api::visible_to(&admin_ev, &admin));
    assert!(!reelix_api::visible_to(&admin_ev, &bob));
    assert!(reelix_api::visible_to(&all_ev, &bob));
}

#[tokio::test]
async fn ui_settings_are_per_user_and_maintenance_is_admin_only() {
    let app = app().await;
    let admin = setup_admin(&app).await;
    create_regular_user(&app).await;
    let bob = login(&app, "bob", "user-password", false).await.cookie();

    let get = |cookie: &str| {
        let cookie = cookie.to_owned();
        let app = &app;
        async move {
            call(
                app,
                Method::GET,
                "/api/v1/settings/ui",
                Req {
                    cookie: Some(&cookie),
                    ..Default::default()
                },
            )
            .await
        }
    };
    let defaults = get(&bob).await;
    assert_eq!(defaults.status, StatusCode::OK);
    assert_eq!(defaults.body["date_format"], "iso");
    assert_eq!(defaults.body["relative_dates"], true);

    // Any signed-in user can save their own; missing fields fall back to the defaults.
    let r = call(
        &app,
        Method::PUT,
        "/api/v1/settings/ui",
        Req {
            cookie: Some(&bob),
            csrf: true,
            body: Some(
                json!({"date_format": "us", "first_day_of_week": "sunday", "calendar_view": "agenda"}),
            ),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let bob_saved = get(&bob).await;
    assert_eq!(bob_saved.body["date_format"], "us");
    assert_eq!(bob_saved.body["first_day_of_week"], "sunday");
    assert_eq!(bob_saved.body["library_view"], "posters");
    assert_eq!(
        get(&admin).await.body["date_format"],
        "iso",
        "settings are per user"
    );

    // Maintenance endpoints are admin-only (checked before the backend is needed).
    for uri in [
        "/api/v1/system/backups",
        "/api/v1/system/logs",
        "/api/v1/system/logs/files",
        "/api/v1/system/updates",
        "/api/v1/system/settings",
    ] {
        let r = call(
            &app,
            Method::GET,
            uri,
            Req {
                cookie: Some(&bob),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{uri}");
    }
    // Installing an update is admin-only too, whatever the body says.
    for body in [json!({"version": "99.0.0"}), json!("not an object")] {
        let r = call(
            &app,
            Method::POST,
            "/api/v1/system/updates/install",
            Req {
                cookie: Some(&bob),
                csrf: true,
                body: Some(body),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{:?}", r.body);
    }

    // A restore upload from a non-admin is refused before any of the body is read.
    let polled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = polled.clone();
    let body = futures::stream::poll_fn(move |_| {
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
        std::task::Poll::Ready(Some(Ok::<_, std::io::Error>(axum::body::Bytes::from_static(
            &[0u8; 1024],
        ))))
    });
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/system/backups/restore")
        .header(header::COOKIE, &bob)
        .header("x-reelix-request", "1")
        .header(header::CONTENT_TYPE, "application/zip")
        .body(Body::from_stream(body))
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert!(
        !polled.load(std::sync::atomic::Ordering::SeqCst),
        "the body was read"
    );
}

fn with_cookie(cookie: &str, csrf: bool) -> Req<'_> {
    Req {
        cookie: Some(cookie),
        csrf,
        ..Default::default()
    }
}

#[tokio::test]
async fn pending_migration_sources_are_admin_only_and_masked() {
    use reelix_api_types::migrate::SourceKind;
    const PENDING: &str = "/api/v1/system/migrate/pending";
    let app = app().await;
    let admin = setup_admin(&app).await;
    create_regular_user(&app).await;
    let bob = login(&app, "bob", "user-password", false).await.cookie();
    let key = "0123456789abcdef0123456789abcdef";
    for (source, url) in [
        (SourceKind::Radarr, "http://localhost:7878"),
        (SourceKind::Sonarr, "http://localhost:8989/sonarr"),
    ] {
        reelix_migrate::pending::add(app.state.db(), app.state.secrets(), source, url, key)
            .await
            .unwrap();
    }

    // Regular users can't see or change them; neither can anonymous callers.
    let r = call(&app, Method::GET, PENDING, with_cookie(&bob, false)).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = call(&app, Method::GET, PENDING, Req::default()).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    for uri in [PENDING, "/api/v1/system/migrate/pending/radarr"] {
        let r = call(&app, Method::DELETE, uri, with_cookie(&bob, true)).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{uri}");
    }
    // A regular user can't borrow a saved key through a connection test either.
    let r = call(
        &app,
        Method::POST,
        "/api/v1/system/migrate/test",
        Req {
            body: Some(json!({"source": "radarr", "url": "http://localhost:7878", "api_key": "********"})),
            ..with_cookie(&bob, true)
        },
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    let r = call(&app, Method::GET, PENDING, with_cookie(&admin, false)).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let list = r.body.as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["source"], "radarr");
    assert_eq!(list[0]["url"], "http://localhost:7878");
    assert_eq!(list[1]["url"], "http://localhost:8989/sonarr");
    assert!(list.iter().all(|s| s["api_key"] == "********"));
    assert!(!r.body.to_string().contains(key));

    let sonarr = "/api/v1/system/migrate/pending/sonarr";
    let r = call(&app, Method::DELETE, sonarr, with_cookie(&admin, true)).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = call(&app, Method::DELETE, sonarr, with_cookie(&admin, true)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = call(
        &app,
        Method::DELETE,
        "/api/v1/system/migrate/pending/nope",
        with_cookie(&admin, true),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = call(&app, Method::DELETE, PENDING, with_cookie(&admin, true)).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = call(&app, Method::GET, PENDING, with_cookie(&admin, false)).await;
    assert_eq!(r.body, json!([]));
}
