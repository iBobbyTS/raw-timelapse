//! SHA-256 for validating native pixel payloads without another full-frame copy.
//! Uses macOS CommonCrypto, the same system authority as the native producer.
#[cfg(target_os = "macos")]
pub fn sha256(bytes: &[u8]) -> String {
    unsafe extern "C" {
        fn CC_SHA256(data: *const u8, length: u32, digest: *mut u8) -> *mut u8;
    }
    let length = u32::try_from(bytes.len()).expect("frame allocation bound exceeds SHA256 limit");
    let mut digest = [0u8; 32];
    // The input slice and output array remain live for the synchronous call.
    unsafe {
        CC_SHA256(bytes.as_ptr(), length, digest.as_mut_ptr());
    }
    digest.iter().map(|b| format!("{b:02x}")).collect()
}
#[cfg(not(target_os = "macos"))]
pub fn sha256(_bytes: &[u8]) -> String {
    // Native ADC/SDK adapter is currently macOS-only.
    panic!("native frame validation requires macOS CommonCrypto")
}
#[cfg(test)]
mod tests {
    #[test]
    #[cfg(target_os = "macos")]
    fn standard_vector() {
        assert_eq!(
            super::sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            super::sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
