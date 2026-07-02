//! Encrypt-for-recipient pipeline для per-device encrypted distribution
//! (MDM-DEVICE-CONTROL-CONTRACT.md §2).
//!
//! Алгоритмы:
//!   * ECDH on NIST P-256 (рекомендуемая Android Keystore curve; client'ский
//!     `KeystoreWrapper` уже генерит ровно эту curve, см. INSIGHT-046).
//!   * HKDF-SHA-256, info = `"outpost-distribution-v1\x00" || file_id ||
//!     recipient_device_id` — derive 32-byte KEK.
//!   * AES-256-GCM AEAD: 12-byte IV, 16-byte tag. Tag хранится отдельно
//!     от ciphertext в БД (separated в схеме `encrypted_distributions`).
//!
//! Public-key wire format: **SEC1 uncompressed point**, 65 bytes
//! (`0x04 || X(32) || Y(32)`). Client (Android Keystore) удобно работает
//! с этим форматом через X509EncodedKeySpec или manual point parsing.
//! Контракт §2.4/§2.5/§2.6 фиксирует именно 65-байтовый формат, не SPKI.

use aes_gcm::aead::{Aead, AeadInPlace, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Context, Result, anyhow};
use hkdf::Hkdf;
use p256::PublicKey;
use p256::ecdh::EphemeralSecret;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use rand_core::{OsRng, RngCore};
use sha2::Sha256;
use zeroize::Zeroizing;

/// Размер DEK (data-encryption key) — 32 bytes (AES-256).
pub const DEK_LEN: usize = 32;
/// Размер KEK — также 32 bytes.
pub const KEK_LEN: usize = 32;
/// AES-GCM IV.
pub const IV_LEN: usize = 12;
/// AES-GCM tag.
pub const TAG_LEN: usize = 16;
/// SEC1 uncompressed P-256 point.
pub const SEC1_UNCOMPRESSED_LEN: usize = 65;

/// HKDF info-prefix (contract-fixed). Сменим только если bump'нем major
/// version всей crypto-схемы.
const HKDF_INFO_PREFIX: &[u8] = b"outpost-distribution-v1\x00";

/// Зашифрованный для одного recipient'а payload.
///
/// Fields соответствуют JSON wire-format'у в push command `fetch-encrypted-file`
/// (контракт §2.5). Для группы admin вызывает [`encrypt_for_recipient`] N раз
/// (по разу на каждого), но **ciphertext blob** должен оставаться один и тот
/// же — потому что DEK переиспользуется (caller-driven, см. ниже).
#[derive(Debug, Clone)]
pub struct RecipientPayload {
    /// Эфемерный public key сервера (65 bytes SEC1 uncompressed).
    pub eph_pubkey_sec1: Vec<u8>,
    /// AES-GCM(KEK, wrapped_dek_iv, DEK) → 32 ciphertext + 16 tag = 48 bytes.
    pub wrapped_dek: Vec<u8>,
    pub wrapped_dek_iv: [u8; IV_LEN],
}

/// Ciphertext + meta для blob'а. Делается **один раз** на distribution, не
/// per-recipient. Caller прокидывает `dek` через [`encrypt_for_recipient`]
/// чтобы wrap пошёл под этот же DEK.
#[derive(Debug, Clone)]
pub struct BlobCiphertext {
    pub ciphertext: Vec<u8>,
    pub iv: [u8; IV_LEN],
    pub tag: [u8; TAG_LEN],
    pub plaintext_sha256_hex: String,
    pub ciphertext_sha256_hex: String,
    /// Длина исходного plaintext'а. Для AES-GCM `ciphertext.len()`
    /// тождественно равна этой величине (GCM не меняет длину — tag detached),
    /// но caller'у нужен явный размер для записи в БД / response, а сам
    /// plaintext-буфер после in-place шифрования уже недоступен.
    pub plaintext_len: usize,
}

/// Encrypt `plaintext` под randomly-generated DEK. Возвращаемый `dek` затем
/// каждый recipient получит через [`encrypt_for_recipient`].
///
/// Принимает `plaintext` **по значению** и шифрует **in-place** в том же
/// буфере (`encrypt_in_place_detached` — tag отдельно, длина не меняется).
/// Это сознательно: аллоцирующий `Aead::encrypt` делал вторую копию размером
/// со весь файл, и для blob'а на 84 МБ пик доходил до ~168 МБ. На проде у
/// systemd-юнита `MemoryMax=256M` — двойная буферизация пробивала cgroup-лимит
/// и ядро убивало процесс OOM-killer'ом (502 на форме «Распространить файл»).
/// In-place шифрование убирает вторую копию: пик ≈ размер файла, не ×2.
/// Wire-формат при этом байт-в-байт прежний (AES-256-GCM, detached tag).
pub fn encrypt_blob(mut plaintext: Vec<u8>) -> Result<(BlobCiphertext, Zeroizing<[u8; DEK_LEN]>)> {
    // DEK в Zeroizing — зануляется при drop'е (у caller'а после per-recipient
    // wrap'а). Ограничения zeroize (копии на стеке/в регистрах) осознаём —
    // это сокращение окна экспозиции, не герметичная гарантия.
    let mut dek = Zeroizing::new([0u8; DEK_LEN]);
    OsRng.fill_bytes(&mut dek[..]);

    let mut iv = [0u8; IV_LEN];
    OsRng.fill_bytes(&mut iv);

    // Хэш и длину plaintext'а фиксируем ДО шифрования — после in-place
    // операции буфер уже содержит ciphertext.
    let plaintext_len = plaintext.len();
    let plain_sha = hex_sha256(&plaintext);

    let cipher = Aes256Gcm::new_from_slice(&dek[..]).context("aes-gcm init")?;
    // In-place AEAD: шифрует `plaintext` на месте, tag возвращается отдельно.
    // Никакой второй аллокации размером с файл.
    let tag_ga = cipher
        .encrypt_in_place_detached(Nonce::from_slice(&iv), b"", &mut plaintext)
        .map_err(|e| anyhow!("aes-gcm encrypt: {e}"))?;
    let mut tag = [0u8; TAG_LEN];
    tag.copy_from_slice(tag_ga.as_slice());
    // Буфер теперь содержит ciphertext (та же длина, что и plaintext).
    let ciphertext = plaintext;

    let cipher_sha = hex_sha256(&ciphertext);

    Ok((
        BlobCiphertext {
            ciphertext,
            iv,
            tag,
            plaintext_sha256_hex: plain_sha,
            ciphertext_sha256_hex: cipher_sha,
            plaintext_len,
        },
        dek,
    ))
}

/// Wrap `dek` под per-recipient ECDH-derived KEK. Возвращает payload который
/// будет встроен в push command `fetch-encrypted-file`.
///
/// `recipient_pubkey_sec1` — 65-байтовый SEC1 uncompressed point, который
/// мы получили в `/api/v1/enroll` request body (поле `device_pubkey.der`).
pub fn encrypt_for_recipient(
    dek: &[u8; DEK_LEN],
    recipient_pubkey_sec1: &[u8],
    file_id: i64,
    recipient_device_id: i64,
) -> Result<RecipientPayload> {
    if recipient_pubkey_sec1.len() != SEC1_UNCOMPRESSED_LEN {
        return Err(anyhow!(
            "recipient pubkey must be {SEC1_UNCOMPRESSED_LEN}-byte SEC1 uncompressed, got {}",
            recipient_pubkey_sec1.len()
        ));
    }
    let recipient_pub = PublicKey::from_sec1_bytes(recipient_pubkey_sec1)
        .map_err(|e| anyhow!("parse recipient pubkey: {e}"))?;

    // Ephemeral server-side keypair — новый на каждого получателя для
    // forward secrecy (compromise одного eph_priv не leaks DEK других).
    let eph_priv = EphemeralSecret::random(&mut OsRng);
    let eph_pub = eph_priv.public_key();
    let eph_sec1 = eph_pub.to_encoded_point(false).as_bytes().to_vec();
    if eph_sec1.len() != SEC1_UNCOMPRESSED_LEN {
        return Err(anyhow!("ephemeral pubkey wrong length: {}", eph_sec1.len()));
    }

    // ECDH → shared secret (32 bytes).
    let shared = eph_priv.diffie_hellman(&recipient_pub);

    // HKDF-SHA-256 expand: info = "outpost-distribution-v1\0" || file_id (ASCII)
    // || recipient_device_id (ASCII). См. контракт §2.2.
    let mut info = Vec::with_capacity(HKDF_INFO_PREFIX.len() + 32);
    info.extend_from_slice(HKDF_INFO_PREFIX);
    info.extend_from_slice(file_id.to_string().as_bytes());
    info.extend_from_slice(recipient_device_id.to_string().as_bytes());

    let hk = Hkdf::<Sha256>::new(None, shared.raw_secret_bytes().as_slice());
    // KEK в Zeroizing — зануляется при выходе из функции. `shared` (p256
    // SharedSecret) сам зануляется на drop'е.
    let mut kek = Zeroizing::new([0u8; KEK_LEN]);
    hk.expand(&info, &mut kek[..])
        .map_err(|e| anyhow!("hkdf expand: {e}"))?;

    let mut wrapped_dek_iv = [0u8; IV_LEN];
    OsRng.fill_bytes(&mut wrapped_dek_iv);

    let cipher = Aes256Gcm::new_from_slice(&kek[..]).context("kek init")?;
    let wrapped_dek = cipher
        .encrypt(
            Nonce::from_slice(&wrapped_dek_iv),
            Payload { msg: dek, aad: b"" },
        )
        .map_err(|e| anyhow!("aes-gcm wrap dek: {e}"))?;
    // wrapped_dek includes 16-byte tag concatenated.

    Ok(RecipientPayload {
        eph_pubkey_sec1: eph_sec1,
        wrapped_dek,
        wrapped_dek_iv,
    })
}

fn hex_sha256(input: &[u8]) -> String {
    use sha2::Digest;
    let digest = Sha256::digest(input);
    let mut out = String::with_capacity(64);
    for b in digest.iter() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::EncodedPoint;
    use p256::SecretKey;
    use p256::elliptic_curve::sec1::FromEncodedPoint;

    /// Test-only mirror of what client (Android Keystore) делает на decrypt:
    /// ECDH с локальной private key, HKDF derive KEK, AES-GCM unwrap DEK,
    /// AES-GCM decrypt blob. Не используется в prod — только для roundtrip
    /// проверки в этом модуле.
    fn decrypt_as_client(
        recipient_secret: &SecretKey,
        payload: &RecipientPayload,
        blob: &BlobCiphertext,
        file_id: i64,
        recipient_device_id: i64,
    ) -> Result<Vec<u8>> {
        // 1. Parse eph_pubkey
        let encoded = EncodedPoint::from_bytes(&payload.eph_pubkey_sec1)
            .map_err(|e| anyhow!("parse eph pubkey: {e}"))?;
        let eph_pub = PublicKey::from_encoded_point(&encoded)
            .into_option()
            .ok_or_else(|| anyhow!("invalid eph pubkey point"))?;

        // 2. ECDH (manual since we have SecretKey not EphemeralSecret)
        let shared =
            p256::ecdh::diffie_hellman(recipient_secret.to_nonzero_scalar(), eph_pub.as_affine());

        // 3. HKDF
        let mut info = Vec::new();
        info.extend_from_slice(HKDF_INFO_PREFIX);
        info.extend_from_slice(file_id.to_string().as_bytes());
        info.extend_from_slice(recipient_device_id.to_string().as_bytes());
        let hk = Hkdf::<Sha256>::new(None, shared.raw_secret_bytes().as_slice());
        let mut kek = [0u8; KEK_LEN];
        hk.expand(&info, &mut kek)
            .map_err(|e| anyhow!("hkdf: {e}"))?;

        // 4. Unwrap DEK
        let kek_cipher = Aes256Gcm::new_from_slice(&kek)?;
        let dek_bytes = kek_cipher
            .decrypt(
                Nonce::from_slice(&payload.wrapped_dek_iv),
                Payload {
                    msg: &payload.wrapped_dek,
                    aad: b"",
                },
            )
            .map_err(|e| anyhow!("unwrap dek: {e}"))?;
        if dek_bytes.len() != DEK_LEN {
            return Err(anyhow!("dek wrong length: {}", dek_bytes.len()));
        }

        // 5. Decrypt blob — ciphertext+tag must be concatenated for aes-gcm crate.
        let mut combined = blob.ciphertext.clone();
        combined.extend_from_slice(&blob.tag);
        let blob_cipher = Aes256Gcm::new_from_slice(&dek_bytes)?;
        let plaintext = blob_cipher
            .decrypt(
                Nonce::from_slice(&blob.iv),
                Payload {
                    msg: &combined,
                    aad: b"",
                },
            )
            .map_err(|e| anyhow!("decrypt blob: {e}"))?;

        Ok(plaintext)
    }

    #[test]
    fn roundtrip_single_recipient() {
        let plaintext = b"Hello tactical world! \xe2\x9a\x94\xef\xb8\x8f";
        let file_id = 42;
        let device_id = 7;

        let recipient_sk = SecretKey::random(&mut OsRng);
        let recipient_pub_sec1 = recipient_sk
            .public_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec();
        assert_eq!(recipient_pub_sec1.len(), 65);

        let (blob, dek) = encrypt_blob(plaintext.to_vec()).unwrap();
        let payload = encrypt_for_recipient(&dek, &recipient_pub_sec1, file_id, device_id).unwrap();

        assert_eq!(payload.eph_pubkey_sec1.len(), 65);
        assert_eq!(payload.wrapped_dek.len(), DEK_LEN + TAG_LEN);

        let decrypted =
            decrypt_as_client(&recipient_sk, &payload, &blob, file_id, device_id).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn wrong_recipient_fails_to_decrypt() {
        let plaintext = b"secret SOP";
        let real_sk = SecretKey::random(&mut OsRng);
        let real_pub = real_sk
            .public_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec();
        let attacker_sk = SecretKey::random(&mut OsRng);

        let (blob, dek) = encrypt_blob(plaintext.to_vec()).unwrap();
        let payload = encrypt_for_recipient(&dek, &real_pub, 1, 1).unwrap();

        // Attacker не может decrypt — другой private key даст другой shared
        // secret → другой KEK → AES-GCM tag mismatch.
        let result = decrypt_as_client(&attacker_sk, &payload, &blob, 1, 1);
        assert!(result.is_err());
    }

    #[test]
    fn wrong_file_id_in_info_fails() {
        // Если кто-то replay'нет payload с другим file_id в info — derivation
        // даст другой KEK и unwrap DEK не пройдёт.
        let plaintext = b"x";
        let recipient_sk = SecretKey::random(&mut OsRng);
        let recipient_pub = recipient_sk
            .public_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec();

        let (blob, dek) = encrypt_blob(plaintext.to_vec()).unwrap();
        let payload = encrypt_for_recipient(&dek, &recipient_pub, 42, 7).unwrap();

        // Decrypt с правильным file_id — OK
        assert!(decrypt_as_client(&recipient_sk, &payload, &blob, 42, 7).is_ok());
        // С другим file_id — fail
        assert!(decrypt_as_client(&recipient_sk, &payload, &blob, 999, 7).is_err());
    }

    #[test]
    fn pubkey_length_check() {
        let dek = [0u8; DEK_LEN];
        let result = encrypt_for_recipient(&dek, b"too short", 1, 1);
        assert!(result.is_err());
    }
}
