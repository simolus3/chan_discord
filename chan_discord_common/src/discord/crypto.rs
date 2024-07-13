use anyhow::anyhow;
use crypto_secretbox::{aead, Tag};
use crypto_secretbox::{aead::AeadInPlace, Nonce, SecretBox, XSalsa20Poly1305 as Cipher};
use discortp::MutablePacket;
use rand::{random, thread_rng, RngCore};
use std::cmp::Ordering;
use std::num::Wrapping;
use std::ops::Range;
use std::str::FromStr;

pub const NONCE_SIZE: usize = SecretBox::<()>::NONCE_SIZE;
pub const TAG_SIZE: usize = SecretBox::<()>::TAG_SIZE;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EncryptionMode {
    Normal,
    Suffix,
    Lite,
}

enum NonceMode {
    Normal,
    Suffix,
    Lite(Wrapping<u32>),
}

pub struct VoiceEncryption {
    cipher: Cipher,
    mode: NonceMode,
}

pub struct VoiceDecryption {
    cipher: Cipher,
    mode: EncryptionMode,
}

impl VoiceEncryption {
    pub const TAG_LEN: usize = TAG_SIZE;
    pub const RTP_HEADER_LEN: usize = 12;

    pub fn new(mode: EncryptionMode, aead: Cipher) -> Self {
        Self {
            mode: match mode {
                EncryptionMode::Normal => NonceMode::Normal,
                EncryptionMode::Suffix => NonceMode::Suffix,
                EncryptionMode::Lite => NonceMode::Lite(random()),
            },
            cipher: aead,
        }
    }

    /// Encrypts a clear-text RTP packet in-place.
    ///
    /// The [packet] must start with an RTP header, followed by a payload beginning with a [TAG_LEN]
    /// padding, [payload_len] of payload data and additional padding bytes used to fill the nonce
    /// depending on the encryption mode.
    ///
    /// Returns the new total length of the packet.
    pub fn encrypt_packet(&mut self, packet: &mut [u8], payload_len: usize) -> aead::Result<usize> {
        let (rtp_header, rtp_payload) = packet.split_at_mut(Self::RTP_HEADER_LEN);
        let (tag_bytes, after_tag) = rtp_payload.split_at_mut(TAG_SIZE);

        let tag = match &mut self.mode {
            NonceMode::Normal => {
                let mut nonce = Nonce::default();
                nonce[0..Self::RTP_HEADER_LEN].copy_from_slice(&rtp_header);
                nonce[Self::RTP_HEADER_LEN..].fill(0);

                self.cipher
                    .encrypt_in_place_detached(&nonce, b"", &mut after_tag[..payload_len])
            }
            NonceMode::Suffix => {
                // Generate 24-byte nonce and append it to the final ciphertext
                let mut nonce = Nonce::default();
                thread_rng().fill_bytes(&mut nonce);

                let res = self.cipher.encrypt_in_place_detached(
                    &nonce,
                    b"",
                    &mut after_tag[..payload_len],
                );

                after_tag[payload_len..(payload_len + 24)].copy_from_slice(&nonce);
                res
            }
            NonceMode::Lite(counter) => {
                let nonce = counter.0;
                *counter += Wrapping(1);

                let nonce_bytes = nonce.to_be_bytes();
                let mut nonce = Nonce::default();
                nonce[0..4].copy_from_slice(&nonce_bytes);
                nonce[4..].fill(0);

                let res = self.cipher.encrypt_in_place_detached(
                    &nonce,
                    b"",
                    &mut after_tag[..payload_len],
                );

                after_tag[payload_len..(payload_len + 4)].copy_from_slice(&nonce_bytes);
                res
            }
        }?;

        tag_bytes.copy_from_slice(&tag);
        Ok(Self::RTP_HEADER_LEN
            + TAG_SIZE
            + payload_len
            + match self.mode {
                NonceMode::Normal => 0,
                NonceMode::Suffix => 24,
                NonceMode::Lite(_) => 4,
            })
    }
}

impl VoiceDecryption {
    pub fn new(mode: EncryptionMode, aead: Cipher) -> Self {
        Self { mode, cipher: aead }
    }

    pub fn min_packet_length(&self) -> usize {
        VoiceEncryption::RTP_HEADER_LEN + TAG_SIZE + self.mode.suffix_len()
    }

    /// Extracts nonce bytes from header or body, returning nonce and the new body.
    fn extract_nonce<'a>(
        &self,
        header: &'a [u8],
        body: &'a mut [u8],
    ) -> anyhow::Result<(&'a [u8], &'a mut [u8])> {
        match self.mode {
            EncryptionMode::Normal => Ok((header, body)),
            EncryptionMode::Suffix | EncryptionMode::Lite => {
                let len = body.len();
                let suffix = self.mode.suffix_len();

                if len < suffix {
                    Err(anyhow!("Body too short to extract nonce"))
                } else {
                    let (body_start, nonce) = body.split_at_mut(len - suffix);
                    Ok((nonce, body_start))
                }
            }
        }
    }

    pub fn decrypt_packet(&self, packet: &mut impl MutablePacket) -> anyhow::Result<Range<usize>> {
        let header_len = packet.packet().len() - packet.payload().len();
        let (header, body) = packet.packet_mut().split_at_mut(header_len);

        let (nonce_bytes, body) = self.extract_nonce(header, body)?;
        let mut nonce_zero = Nonce::default();
        let nonce = if nonce_bytes.len() == NONCE_SIZE {
            Nonce::from_slice(nonce_bytes)
        } else {
            nonce_zero[..nonce_bytes.len()].copy_from_slice(nonce_bytes);
            &nonce_zero
        };

        if body.len() < TAG_SIZE {
            return Err(anyhow!("Body too short"));
        }

        let (tag_bytes, ciphertext_bytes) = body.split_at_mut(TAG_SIZE);

        self.cipher
            .decrypt_in_place_detached(&nonce, b"", ciphertext_bytes, &Tag::from_slice(tag_bytes))
            .map_err(|e| anyhow!("Could not decrypt: {e}"))?;

        let body_start = header_len + TAG_SIZE;
        let body_end = body_start + ciphertext_bytes.len();
        Ok(body_start..body_end)
    }
}

impl EncryptionMode {
    pub fn name(&self) -> &'static str {
        match self {
            EncryptionMode::Normal => "xsalsa20_poly1305",
            EncryptionMode::Suffix => "xsalsa20_poly1305_suffix",
            EncryptionMode::Lite => "xsalsa20_poly1305_lite",
        }
    }

    fn suffix_len(&self) -> usize {
        match self {
            EncryptionMode::Normal => 0,
            EncryptionMode::Suffix => 24,
            EncryptionMode::Lite => 4,
        }
    }

    fn effective_nonce_entropy(&self) -> usize {
        match self {
            EncryptionMode::Normal => 4,
            EncryptionMode::Suffix => 24,
            EncryptionMode::Lite => 4,
        }
    }
}

impl FromStr for EncryptionMode {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        return match s {
            "xsalsa20_poly1305" => Ok(EncryptionMode::Normal),
            "xsalsa20_poly1305_suffix" => Ok(EncryptionMode::Suffix),
            "xsalsa20_poly1305_lite" => Ok(EncryptionMode::Lite),
            _ => Err(()),
        };
    }
}

impl PartialOrd for EncryptionMode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for EncryptionMode {
    fn cmp(&self, other: &Self) -> Ordering {
        self.effective_nonce_entropy()
            .cmp(&other.effective_nonce_entropy())
    }
}
