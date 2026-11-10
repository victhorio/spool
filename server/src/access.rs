use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use serde::Deserialize;

use crate::error::ApiError;

const REFRESH: Duration = Duration::from_secs(30);
const MAX_AGE: Duration = Duration::from_secs(60 * 60);
const LEEWAY: i64 = 30;
const MAX_ASSERTION: usize = 16 * 1024;
const MAX_CERTS: usize = 64 * 1024;

/// A Cloudflare Access application. Browser routes require its signed assertion,
/// because the origin remains reachable without passing through Access.
#[derive(Clone)]
pub struct Access(Arc<Inner>);

struct Inner {
    issuer: String,
    audience: String,
    certs: String,
    client: reqwest::Client,
    keys: Mutex<Keys>,
}

type Key = Arc<RsaPublicKeyComponents<Vec<u8>>>;

#[derive(Default)]
struct Keys {
    by_id: HashMap<String, Key>,
    fetched: Option<Instant>,
    attempted: Option<Instant>,
    failed: bool,
}

enum Rejection {
    Invalid,
    Unavailable,
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    kid: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
struct Claims {
    iss: String,
    aud: Audience,
    exp: i64,
    nbf: Option<i64>,
}

#[derive(Deserialize)]
struct Certs {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kid: String,
    kty: String,
    n: Option<String>,
    e: Option<String>,
}

impl Access {
    /// `issuer` is the team domain origin, such as `https://team.cloudflareaccess.com`;
    /// `audience` is the application's AUD tag.
    pub fn new(issuer: &str, audience: &str) -> anyhow::Result<Self> {
        let url = reqwest::Url::parse(issuer).context("Access issuer must be a URL")?;
        let loopback = url
            .host_str()
            .and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback());
        if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
        {
            bail!(
                "Access issuer must be an HTTPS origin, such as https://team.cloudflareaccess.com"
            );
        }
        if audience.is_empty()
            || audience.len() > 256
            || !audience.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            bail!("Access audience must be the application's AUD tag");
        }
        let issuer = url.origin().ascii_serialization();
        Ok(Self(Arc::new(Inner {
            certs: format!("{issuer}/cdn-cgi/access/certs"),
            issuer,
            audience: audience.to_owned(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            keys: Mutex::default(),
        })))
    }

    async fn verify(&self, assertion: &str) -> Result<(), Rejection> {
        if assertion.len() > MAX_ASSERTION {
            return Err(Rejection::Invalid);
        }
        let mut parts = assertion.split('.');
        let (Some(header), Some(payload), Some(signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(Rejection::Invalid);
        };
        let header: Header = decode(header)?;
        if header.alg != "RS256" {
            return Err(Rejection::Invalid);
        }
        let key = self.key(&header.kid).await?.ok_or(Rejection::Invalid)?;
        let signed = &assertion.as_bytes()[..assertion.len() - signature.len() - 1];
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| Rejection::Invalid)?;
        key.verify(&RSA_PKCS1_2048_8192_SHA256, signed, &signature)
            .map_err(|_| Rejection::Invalid)?;
        let claims: Claims = decode(payload)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Rejection::Unavailable)?
            .as_secs() as i64;
        let audience = match &claims.aud {
            Audience::One(value) => value == &self.0.audience,
            Audience::Many(values) => values.contains(&self.0.audience),
        };
        if claims.iss != self.0.issuer
            || !audience
            || claims.exp.saturating_add(LEEWAY) <= now
            || claims
                .nbf
                .is_some_and(|nbf| nbf.saturating_sub(LEEWAY) > now)
        {
            return Err(Rejection::Invalid);
        }
        Ok(())
    }

    /// Keys refresh when an assertion names an unknown key or the set is an hour old,
    /// at most once per interval. Fetches happen outside the lock; if one fails,
    /// already cached keys remain usable.
    async fn key(&self, id: &str) -> Result<Option<Key>, Rejection> {
        let cached = {
            let mut keys = self.0.keys.lock().expect("key cache lock");
            let cached = keys.by_id.get(id).cloned();
            let fresh = keys
                .fetched
                .is_some_and(|fetched| fetched.elapsed() < MAX_AGE);
            if cached.is_some() && fresh {
                return Ok(cached);
            }
            if keys
                .attempted
                .is_some_and(|attempted| attempted.elapsed() < REFRESH)
            {
                return match cached {
                    Some(key) => Ok(Some(key)),
                    None if keys.failed => Err(Rejection::Unavailable),
                    None => Ok(None),
                };
            }
            keys.attempted = Some(Instant::now());
            cached
        };
        let fetched = self.fetch().await;
        let mut keys = self.0.keys.lock().expect("key cache lock");
        match fetched {
            Ok(by_id) => {
                keys.by_id = by_id;
                keys.fetched = Some(Instant::now());
                keys.failed = false;
                Ok(keys.by_id.get(id).cloned())
            }
            Err(error) => {
                keys.failed = true;
                eprintln!(
                    "{}",
                    serde_json::json!({"event": "access_certs_unavailable", "error": error.to_string()})
                );
                cached.map(Some).ok_or(Rejection::Unavailable)
            }
        }
    }

    async fn fetch(&self) -> anyhow::Result<HashMap<String, Arc<RsaPublicKeyComponents<Vec<u8>>>>> {
        let response = self
            .0
            .client
            .get(&self.0.certs)
            .send()
            .await?
            .error_for_status()?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_CERTS as u64)
        {
            bail!("certificate response too large");
        }
        let body = response.bytes().await?;
        if body.len() > MAX_CERTS {
            bail!("certificate response too large");
        }
        let certs: Certs = serde_json::from_slice(&body)?;
        let keys: HashMap<_, _> = certs
            .keys
            .into_iter()
            .filter(|key| key.kty == "RSA")
            .filter_map(|key| {
                let n = URL_SAFE_NO_PAD.decode(key.n?).ok()?;
                let e = URL_SAFE_NO_PAD.decode(key.e?).ok()?;
                Some((key.kid, Arc::new(RsaPublicKeyComponents { n, e })))
            })
            .collect();
        if keys.is_empty() {
            bail!("no RSA signing keys");
        }
        Ok(keys)
    }
}

fn decode<T: for<'de> Deserialize<'de>>(part: &str) -> Result<T, Rejection> {
    let bytes = URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| Rejection::Invalid)?;
    serde_json::from_slice(&bytes).map_err(|_| Rejection::Invalid)
}

pub(crate) async fn require(
    State(access): State<Access>,
    request: Request,
    next: Next,
) -> Response {
    let mut values = request.headers().get_all("cf-access-jwt-assertion").iter();
    let assertion = match (values.next().map(|value| value.to_str()), values.next()) {
        (Some(Ok(value)), None) => value,
        _ => return ApiError::new(StatusCode::FORBIDDEN, "forbidden").into_response(),
    };
    match access.verify(assertion).await {
        Ok(()) => next.run(request).await,
        Err(Rejection::Invalid) => {
            ApiError::new(StatusCode::FORBIDDEN, "forbidden").into_response()
        }
        Err(Rejection::Unavailable) => {
            ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "access_unavailable").into_response()
        }
    }
}
