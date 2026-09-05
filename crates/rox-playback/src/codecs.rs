//! The one place the codec set is assembled. Symphonia's global
//! `default::get_codecs()` holds exactly the codecs its own features enable,
//! and rox needs one more than that: the Opus decoder in [`crate::opus`].
//! Rather than remember to reach past the global at every call site, this
//! module builds a registry once, feature-enabled codecs plus ours, and hands
//! back the same one forever.
//!
//! That the registry is shared is the point. Playback, the ReplayGain
//! measurement pass, and the acoustic extractor all decode the same files, and
//! a file that plays but can't be analyzed (or the reverse) is a bug that only
//! shows up on somebody's library months later. One registry means they can't
//! disagree about what decodes.
//!
//! The probe stays on `symphonia::default::get_probe()`. Containers aren't the
//! gap here: the Ogg reader already maps Opus, it just had nothing to hand the
//! packets to.

use std::sync::OnceLock;

use symphonia::core::codecs::registry::CodecRegistry;

/// The codec registry every decode in rox goes through.
pub fn registry() -> &'static CodecRegistry {
    static REGISTRY: OnceLock<CodecRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut reg = CodecRegistry::new();
        symphonia::default::register_enabled_codecs(&mut reg);
        reg.register_audio_decoder::<crate::opus::OpusDecoder>();
        reg
    })
}

#[cfg(test)]
mod tests {
    use symphonia::core::codecs::audio::well_known::{CODEC_ID_FLAC, CODEC_ID_OPUS};

    /// The registry is symphonia's set plus ours, not ours instead of theirs.
    #[test]
    fn the_registry_holds_opus_beside_the_built_in_codecs() {
        let reg = super::registry();
        assert!(
            reg.get_audio_decoder(CODEC_ID_OPUS).is_some(),
            "the whole reason this registry exists"
        );
        assert!(
            reg.get_audio_decoder(CODEC_ID_FLAC).is_some(),
            "and symphonia's own codecs are still there"
        );
    }
}
