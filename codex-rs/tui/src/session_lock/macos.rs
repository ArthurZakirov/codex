//! Read the lock state of the calling process's macOS login session.

use chrono::Utc;
use core_foundation::array::CFArray;
use core_foundation::base::CFType;
use core_foundation::base::CFTypeRef;
use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::dictionary::CFDictionaryRef;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGSessionCopyCurrentDictionary() -> CFDictionaryRef;
}

pub(super) fn is_locked() -> anyhow::Result<bool> {
    // SAFETY: The function takes no arguments and returns an owned dictionary or null.
    let session = unsafe { CGSessionCopyCurrentDictionary() };
    if session.is_null() {
        return console_session_locked();
    }
    // SAFETY: CGSession follows the CF create rule. The dictionary owns its CF strings and values.
    let session: CFDictionary<CFString, CFType> =
        unsafe { TCFType::wrap_under_create_rule(session) };
    Ok(requires_authentication(&session, Utc::now().timestamp()))
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IORegistryGetRootEntry(port: u32) -> u32;
    fn IOObjectRelease(object: u32) -> i32;
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: core_foundation::string::CFStringRef,
        allocator: core_foundation::base::CFAllocatorRef,
        options: u32,
    ) -> CFTypeRef;
}

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SessionGetInfo(session: u32, actual: *mut u32, attributes: *mut u32) -> i32;
}

fn console_session_locked() -> anyhow::Result<bool> {
    let mut session_id = 0;
    // SAFETY: callerSecuritySession requests our own security session.
    let status = unsafe { SessionGetInfo(u32::MAX, &mut session_id, std::ptr::null_mut()) };
    anyhow::ensure!(status == 0 && session_id != 0, "unknown security session");
    // SAFETY: zero is the default IOKit main port.
    let root = unsafe { IORegistryGetRootEntry(0) };
    anyhow::ensure!(root != 0, "IORegistry root unavailable");
    let property = |key: &str| {
        let key = CFString::new(key);
        // SAFETY: root and key are live; null selects the default allocator.
        let raw = unsafe {
            IORegistryEntryCreateCFProperty(root, key.as_concrete_TypeRef(), std::ptr::null(), 0)
        };
        // SAFETY: a non-null Create result is owned.
        (!raw.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(raw) })
    };
    let users = property("IOConsoleUsers");
    let locked = property("IOConsoleLocked");
    // SAFETY: release the owned root after both property reads.
    unsafe { IOObjectRelease(root) };
    let users = users
        .and_then(|v| v.downcast::<CFArray>())
        .ok_or_else(|| anyhow::anyhow!("console sessions unavailable"))?;
    let locked = locked
        .and_then(|v| v.downcast::<CFBoolean>())
        .ok_or_else(|| anyhow::anyhow!("console lock state unavailable"))?;
    // SAFETY: geteuid takes no arguments and returns the effective uid.
    let uid = unsafe { libc::geteuid() };
    console_locked_for_session(&users, bool::from(locked), session_id, uid)
}

fn console_locked_for_session(
    users: &CFArray,
    locked: bool,
    session_id: u32,
    uid: u32,
) -> anyhow::Result<bool> {
    if locked {
        return Ok(true);
    }
    for raw in users.iter() {
        // SAFETY: IOConsoleUsers is a Core Foundation array retaining its members.
        let value = unsafe { CFType::wrap_under_get_rule(*raw) };
        let Some(dict) = value.downcast::<CFDictionary>() else {
            continue;
        };
        // SAFETY: keys are looked up only with CFStrings; values are checked before use.
        let dict: CFDictionary<CFString, CFType> =
            unsafe { TCFType::wrap_under_get_rule(dict.as_concrete_TypeRef()) };
        let number = |key| {
            dict.find(CFString::new(key))
                .and_then(|v| v.downcast::<CFNumber>())
                .and_then(|v| v.to_i64())
        };
        let flag = |key| {
            dict.find(CFString::new(key))
                .and_then(|v| v.downcast::<CFBoolean>())
                .map(bool::from)
        };
        if number("kCGSSessionAuditIDKey") == Some(i64::from(session_id))
            && number("kCGSSessionUserIDKey") == Some(i64::from(uid))
            && flag("kCGSSessionOnConsoleKey") == Some(true)
            && flag("kCGSessionLoginDoneKey") == Some(true)
        {
            return Ok(requires_authentication(&dict, Utc::now().timestamp()));
        }
    }
    anyhow::bail!("current login session is not the active console session")
}

fn requires_authentication(session: &CFDictionary<CFString, CFType>, now: i64) -> bool {
    let lock_screen_active = session
        .find(CFString::new("CGSSessionScreenIsLocked"))
        .and_then(|locked| locked.downcast::<CFBoolean>())
        .is_some_and(bool::from);
    // loginwindow sets the flag as soon as the saver starts. XNU enforces the password
    // after this per-session Unix timestamp, including when the grace period expires.
    let password_required_at = session
        .find(CFString::new("CGSSessionScreenLockedTime"))
        .and_then(|time| time.downcast::<CFNumber>())
        .and_then(|time| time.to_i64());
    lock_screen_active && password_required_at.is_some_and(|time| (1..=now).contains(&time))
}

#[cfg(test)]
#[path = "macos_tests.rs"]
mod tests;
