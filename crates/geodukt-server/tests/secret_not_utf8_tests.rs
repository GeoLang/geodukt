#![cfg(unix)]

use geodukt_server::auth::{AuthConfig, SECRET_ENV};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

#[test]
fn a_non_utf8_secret_is_an_error_naming_the_variable() {
    // PLATFORM_JWT_SECRET is process-wide and this binary has one test
    unsafe { std::env::set_var(SECRET_ENV, OsStr::from_bytes(&[0xff])) };
    let error = AuthConfig::from_env().unwrap_err();
    assert_eq!(
        error.to_string(),
        "PLATFORM_JWT_SECRET is set but not UTF-8"
    );
}
