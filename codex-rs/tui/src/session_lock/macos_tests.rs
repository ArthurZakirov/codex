#![cfg(test)]

use super::*;
use pretty_assertions::assert_eq;

#[test]
fn screen_saver_stops_voice_when_its_password_grace_period_expires() {
    let password_required_at = 1_793_735_319_i64;
    let session = CFDictionary::from_CFType_pairs(&[
        (
            CFString::new("CGSSessionScreenIsLocked"),
            CFBoolean::true_value().as_CFType(),
        ),
        (
            CFString::new("CGSSessionScreenLockedTime"),
            CFNumber::from(password_required_at).as_CFType(),
        ),
    ]);
    let observed = [password_required_at - 1, password_required_at]
        .map(|now| requires_authentication(&session, now));
    assert_eq!(observed, [false, true]);
}

#[test]
fn fallback_requires_the_same_active_login_and_an_unlocked_console() {
    let session = CFDictionary::from_CFType_pairs(&[
        (
            CFString::new("kCGSSessionAuditIDKey"),
            CFNumber::from(17).as_CFType(),
        ),
        (
            CFString::new("kCGSSessionUserIDKey"),
            CFNumber::from(501).as_CFType(),
        ),
        (
            CFString::new("kCGSSessionOnConsoleKey"),
            CFBoolean::true_value().as_CFType(),
        ),
        (
            CFString::new("kCGSessionLoginDoneKey"),
            CFBoolean::true_value().as_CFType(),
        ),
    ]);
    let users = CFArray::from_CFTypes(&[session.as_CFType()]).into_untyped();
    assert!(console_locked_for_session(&users, true, 17, 501).unwrap());
    assert!(!console_locked_for_session(&users, false, 17, 501).unwrap());
    assert!(console_locked_for_session(&users, false, 18, 501).is_err());
    assert!(console_locked_for_session(&users, false, 17, 502).is_err());
}
