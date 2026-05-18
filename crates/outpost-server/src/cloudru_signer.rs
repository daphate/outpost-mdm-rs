//! Cloud.ru SigV4 presigned-URL generator.
//!
//! Read-only download URLs для bucket'а с APK / моделями / прочими публикуемыми
//! артефактами. Алгоритм — стандартный AWS Signature Version 4 query-string
//! signing, со спецификой Cloud.ru:
//!
//!   - **Композитный access key** `<tenant_id>:<key_id>` — Cloud.ru требует
//!     указывать tenant даже для anonymous-public-read объектов. Bucket policy
//!     "*" без tenant'а возвращает 400 «missing tenant id».
//!   - **Region** `ru-central-1`, **service** `s3`, **endpoint** `s3.cloud.ru`,
//!     **path-style** addressing (`https://s3.cloud.ru/<bucket>/<key>`).
//!
//! Алгоритм byte-for-byte идентичен Kotlin'овскому `CloudRuSigner.kt`
//! из outpost-android, который в свою очередь verified против boto3
//! (`.tmp/sigv4_parity_test.py` в tactical-ar-hud). Golden vector в
//! unit-тесте ниже сгенерирован stand-alone Python скриптом без boto3
//! (`.tmp/gen_golden_vector.py`).
//!
//! ## Usage
//!
//! ```ignore
//! let presigner = CloudRuPresigner::new(tenant_id, key_id, secret);
//! let url = presigner.presigned_get_url("apks/outpost-latest-debug.apk", 604_800);
//! // url валиден 7 дней
//! ```
//!
//! ## SigV4 TTL ограничения
//!
//! Query-string presigned URLs ограничены [1, 604800] секундами (7 дней).
//! За пределами этого диапазона `presigned_get_url` паникует. AWS-спека:
//! <https://docs.aws.amazon.com/general/latest/gr/sigv4-create-canonical-request.html>

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// AWS Signature Version 4 max-TTL для query-presigned URLs.
pub const SIGV4_MAX_EXPIRES_SECS: u64 = 604_800;

const ALGORITHM: &str = "AWS4-HMAC-SHA256";
const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";
const DEFAULT_REGION: &str = "ru-central-1";
const DEFAULT_SERVICE: &str = "s3";
const DEFAULT_HOST: &str = "s3.cloud.ru";
const DEFAULT_BUCKET: &str = "outpost";

/// Cloud.ru SigV4 presigner. Holds read-only IAM creds and generates time-bounded
/// download URLs. Cheap to clone (всё `String`'и).
#[derive(Debug, Clone)]
pub struct CloudRuPresigner {
    tenant_id: String,
    key_id: String,
    secret: String,
    region: String,
    service: String,
    host: String,
    bucket: String,
}

impl CloudRuPresigner {
    /// Construct presigner с defaults для Cloud.ru `outpost` bucket в `ru-central-1`.
    pub fn new(
        tenant_id: impl Into<String>,
        key_id: impl Into<String>,
        secret: impl Into<String>,
    ) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            key_id: key_id.into(),
            secret: secret.into(),
            region: DEFAULT_REGION.to_string(),
            service: DEFAULT_SERVICE.to_string(),
            host: DEFAULT_HOST.to_string(),
            bucket: DEFAULT_BUCKET.to_string(),
        }
    }

    /// Override bucket (default `outpost`). Полезно для тестов или для других
    /// артефакт-bucket'ов того же tenant'а.
    pub fn with_bucket(mut self, bucket: impl Into<String>) -> Self {
        self.bucket = bucket.into();
        self
    }

    /// Override region (default `ru-central-1`).
    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        self.region = region.into();
        self
    }

    /// Override host (default `s3.cloud.ru`).
    pub fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    /// Read-only accessors. Used by /api/v1/enroll handler чтобы прокинуть
    /// те же creds в device-side ModelPreferences через MDM override flow
    /// (MDM-DEPLOY-CONTRACT §1.5). НЕ логировать — это secrets.
    pub fn tenant_id(&self) -> &str { &self.tenant_id }
    pub fn key_id(&self) -> &str { &self.key_id }
    pub fn secret(&self) -> &str { &self.secret }

    /// Сгенерировать presigned GET URL для `key` (e.g. `apks/latest/app-debug.apk`).
    /// `expires_in_seconds` должен быть в `[1, 604800]`.
    ///
    /// # Panics
    ///
    /// Если `expires_in_seconds` вне диапазона. SigV4-spec не допускает
    /// большие/меньшие значения — лучше panic чем сгенерить URL который
    /// сервер потом отвергнет с opaque 403.
    pub fn presigned_get_url(&self, key: &str, expires_in_seconds: u64) -> String {
        self.presigned_get_url_at(key, expires_in_seconds, Utc::now())
    }

    /// Testable variant — accepts explicit `now`. Production callers всегда
    /// используют [`Self::presigned_get_url`] который подставляет `Utc::now()`.
    pub fn presigned_get_url_at(
        &self,
        key: &str,
        expires_in_seconds: u64,
        now: DateTime<Utc>,
    ) -> String {
        assert!(
            (1..=SIGV4_MAX_EXPIRES_SECS).contains(&expires_in_seconds),
            "expires_in_seconds={expires_in_seconds} out of SigV4 range [1, {SIGV4_MAX_EXPIRES_SECS}]"
        );

        let access_key = format!("{}:{}", self.tenant_id, self.key_id);
        let datetime = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date = &datetime[..8];
        let scope = format!("{}/{}/{}/aws4_request", date, self.region, self.service);
        let credential = format!("{}/{}", access_key, scope);

        // Canonical URI — path-style, slash в имени bucket'а / key НЕ кодируем.
        let canonical_uri = format!(
            "/{}/{}",
            uri_encode(&self.bucket, /* encode_slash */ false),
            uri_encode(key, /* encode_slash */ false),
        );

        // Query params — отсортированы лексикографически по ключу (AWS требует).
        let mut params: Vec<(&str, String)> = vec![
            ("X-Amz-Algorithm", ALGORITHM.to_string()),
            ("X-Amz-Credential", credential),
            ("X-Amz-Date", datetime.clone()),
            ("X-Amz-Expires", expires_in_seconds.to_string()),
            ("X-Amz-SignedHeaders", "host".to_string()),
        ];
        params.sort_by(|a, b| a.0.cmp(b.0));

        // В query *значения* кодируем со slash (`/` → `%2F`), это match'ит boto3.
        let canonical_query = params
            .iter()
            .map(|(k, v)| {
                format!(
                    "{}={}",
                    uri_encode(k, /* encode_slash */ true),
                    uri_encode(v, /* encode_slash */ true),
                )
            })
            .collect::<Vec<_>>()
            .join("&");

        let canonical_headers = format!("host:{}\n", self.host);
        let signed_headers = "host";

        // Canonical request: METHOD\nURI\nQuery\nHeaders\n\nSignedHeaders\nPayloadHash
        // (между headers и signed_headers стоит пустая строка-разделитель — она в
        //  canonical_headers как trailing \n + \n из format!()).
        let canonical_request = format!(
            "GET\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{UNSIGNED_PAYLOAD}"
        );
        let canonical_request_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));

        let string_to_sign =
            format!("{ALGORITHM}\n{datetime}\n{scope}\n{canonical_request_hash}");

        // Derive signing key через 4 HMAC-SHA256 шага.
        let k_date = hmac_sha256(format!("AWS4{}", self.secret).as_bytes(), date.as_bytes());
        let k_region = hmac_sha256(&k_date, self.region.as_bytes());
        let k_service = hmac_sha256(&k_region, self.service.as_bytes());
        let k_signing = hmac_sha256(&k_service, b"aws4_request");
        let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes()));

        format!(
            "https://{host}{canonical_uri}?{canonical_query}&X-Amz-Signature={signature}",
            host = self.host,
        )
    }
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// AWS SigV4 URI encoding. Unreserved characters (`A-Z a-z 0-9 - . _ ~`)
/// pass through; всё остальное процентно-кодируется (UTF-8 bytes).
///
/// `encode_slash`:
///   - `false` для canonical URI path (`/bucket/key` — slash сохраняется)
///   - `true` для значений в canonical query string (`/` → `%2F`,
///     match'ит boto3 / AWS Java SDK поведение)
fn uri_encode(input: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(input.len() * 2);
    for b in input.bytes() {
        let is_unreserved = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'.' | b'_' | b'~');
        match (is_unreserved, b, encode_slash) {
            (true, _, _) => out.push(b as char),
            (false, b'/', false) => out.push('/'),
            _ => {
                use std::fmt::Write;
                // Uppercase hex per AWS spec (e.g. `%2F` not `%2f`).
                write!(&mut out, "%{:02X}", b).expect("write to String never fails");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// Golden vector — output идентичен Python reference implementation в
    /// `.tmp/gen_golden_vector.py`, который byte-for-byte matches Kotlin
    /// `CloudRuSigner.kt`, который verified against boto3 в
    /// `.tmp/sigv4_parity_test.py` (tactical-ar-hud).
    ///
    /// Дummy creds (tenant=1...5, key=a*32, secret=b*32) специально выбраны
    /// чтобы случайно не зашить production read-only ключи в Rust test code.
    #[test]
    fn golden_vector_matches_python_reference() {
        let presigner = CloudRuPresigner::new(
            "11111111-2222-3333-4444-555555555555",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        );
        let now = Utc.with_ymd_and_hms(2026, 5, 18, 0, 0, 0).unwrap();
        let url = presigner.presigned_get_url_at("apks/latest/app-debug.apk", 604_800, now);
        let expected = "https://s3.cloud.ru/outpost/apks/latest/app-debug.apk?\
            X-Amz-Algorithm=AWS4-HMAC-SHA256&\
            X-Amz-Credential=11111111-2222-3333-4444-555555555555%3Aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa%2F20260518%2Fru-central-1%2Fs3%2Faws4_request&\
            X-Amz-Date=20260518T000000Z&\
            X-Amz-Expires=604800&\
            X-Amz-SignedHeaders=host&\
            X-Amz-Signature=07e8a745ba370e72a92a1c15d05eec360e749046d300052f27b77d0292409e69";
        assert_eq!(url, expected, "golden vector mismatch");
    }

    #[test]
    fn signature_is_64_hex_lowercase() {
        let p = CloudRuPresigner::new("t", "k", "s");
        let url = p.presigned_get_url("apks/latest/app-debug.apk", 3600);
        let sig = url
            .split("X-Amz-Signature=")
            .nth(1)
            .expect("signature param present");
        assert_eq!(sig.len(), 64);
        assert!(sig.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    #[test]
    fn different_keys_produce_different_signatures() {
        let p = CloudRuPresigner::new("t", "k", "s");
        let now = Utc.with_ymd_and_hms(2026, 5, 18, 0, 0, 0).unwrap();
        let u1 = p.presigned_get_url_at("apks/a.apk", 3600, now);
        let u2 = p.presigned_get_url_at("apks/b.apk", 3600, now);
        let sig = |u: &str| {
            u.split("X-Amz-Signature=")
                .nth(1)
                .unwrap()
                .to_string()
        };
        assert_ne!(sig(&u1), sig(&u2));
    }

    #[test]
    fn key_with_spaces_is_percent_encoded() {
        let p = CloudRuPresigner::new("t", "k", "s");
        let url = p.presigned_get_url("apks/build 38/app.apk", 3600);
        assert!(url.contains("/apks/build%2038/app.apk"), "spaces must be %20-encoded; got {url}");
    }

    #[test]
    fn uri_encode_keeps_unreserved_chars() {
        assert_eq!(uri_encode("AZaz09-._~", false), "AZaz09-._~");
        assert_eq!(uri_encode("AZaz09-._~", true), "AZaz09-._~");
    }

    #[test]
    fn uri_encode_handles_slash_based_on_flag() {
        assert_eq!(uri_encode("a/b/c", false), "a/b/c");
        assert_eq!(uri_encode("a/b/c", true), "a%2Fb%2Fc");
    }

    #[test]
    fn uri_encode_uppercase_hex_per_spec() {
        // %2F not %2f — verified against AWS spec.
        assert_eq!(uri_encode("/", true), "%2F");
        assert_eq!(uri_encode(":", true), "%3A");
    }

    #[test]
    #[should_panic(expected = "out of SigV4 range")]
    fn rejects_zero_expires() {
        let p = CloudRuPresigner::new("t", "k", "s");
        p.presigned_get_url("apks/latest/app-debug.apk", 0);
    }

    #[test]
    #[should_panic(expected = "out of SigV4 range")]
    fn rejects_above_7day_expires() {
        let p = CloudRuPresigner::new("t", "k", "s");
        p.presigned_get_url("apks/latest/app-debug.apk", SIGV4_MAX_EXPIRES_SECS + 1);
    }

    #[test]
    fn with_bucket_changes_url_path() {
        let p = CloudRuPresigner::new("t", "k", "s").with_bucket("other-bucket");
        let url = p.presigned_get_url("foo.apk", 3600);
        assert!(url.contains("/other-bucket/foo.apk"), "{url}");
    }
}
