//! The new app's code signature. Every new app must be validly signed,
//! nested code included, under a Developer ID requirement. When the running
//! app has a Team ID, that requirement names the same team and the update
//! may install itself. An ad-hoc signed running app has no team to match,
//! so it accepts any Developer ID team and leaves installing to the user.

use std::path::Path;

/// Why a new app's signature was refused, or may not install itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CodeError {
    #[error("this copy of Crossbow is ad-hoc signed, so it has no Team ID an update could match")]
    AdHoc,
    #[error("the new app's code signature is not valid")]
    Invalid,
    #[error("the new app is not signed with this app's Team ID")]
    Team,
    #[error("the new app is not signed with a Developer ID")]
    DeveloperId,
}

/// Reads code signatures; the system's, or a stand-in in tests.
pub trait CodeCheck: Send + Sync {
    /// The running app's Team ID; None when it is ad-hoc signed or unsigned.
    fn running_team(&self) -> Option<String>;
    /// The Team ID `app`'s signature names, valid or not.
    fn signer(&self, app: &Path) -> Option<String>;
    /// Whether `app`'s signature is valid, nested code included.
    fn valid(&self, app: &Path) -> bool;
    /// Whether `app` meets [`requirement`] for `team`.
    fn meets(&self, app: &Path, team: &str) -> bool;
}

/// A new app's signature, checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signed {
    /// The Developer ID team that validly signed it.
    pub team: String,
    /// Whether it may install itself: not when this app is ad-hoc signed.
    pub install: Result<(), CodeError>,
}

/// Check the new app at `app`: a valid signature under a Developer ID, for
/// the running app's own team when it has one.
pub fn check(code: &dyn CodeCheck, app: &Path) -> Result<Signed, CodeError> {
    if !code.valid(app) {
        return Err(CodeError::Invalid);
    }
    let Some(team) = code.running_team() else {
        let team = code.signer(app).filter(|team| requirement(team).is_some() && code.meets(app, team)).ok_or(CodeError::DeveloperId)?;
        return Ok(Signed { team, install: Err(CodeError::AdHoc) });
    };
    if requirement(&team).is_none() || !code.meets(app, &team) {
        return Err(CodeError::Team);
    }
    Ok(Signed { team, install: Ok(()) })
}

/// A Developer ID application signed by `team` (ten letters and digits).
pub fn requirement(team: &str) -> Option<String> {
    let well_formed = team.len() == 10 && team.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    well_formed.then(|| {
        format!(
            "identifier \"{}\" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"{team}\"",
            super::archive::BUNDLE_ID
        )
    })
}

/// The Security framework's answers.
pub struct System;

impl CodeCheck for System {
    fn running_team(&self) -> Option<String> {
        #[cfg(target_os = "macos")]
        return mac::running_team();
        #[cfg(not(target_os = "macos"))]
        None
    }

    fn signer(&self, app: &Path) -> Option<String> {
        team_of(app)
    }

    fn valid(&self, app: &Path) -> bool {
        #[cfg(target_os = "macos")]
        return mac::valid(app, None);
        #[cfg(not(target_os = "macos"))]
        {
            let _ = app;
            false
        }
    }

    fn meets(&self, app: &Path, team: &str) -> bool {
        #[cfg(target_os = "macos")]
        return requirement(team).is_some_and(|requirement| mac::valid(app, Some(&requirement)));
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (app, team);
            false
        }
    }
}

/// The Team ID in the signature of the code at `path`; None when unsigned,
/// ad-hoc signed or unreadable.
pub fn team_of(path: &Path) -> Option<String> {
    #[cfg(target_os = "macos")]
    return mac::team_of(path);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        None
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{CStr, c_char, c_void};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr::null;

    type Ref = *const c_void;

    const UTF8: u32 = 0x0800_0100;
    const SIGNING_INFORMATION: u32 = 1 << 1;
    /// All architectures, nested code, strict, symlinks kept inside, no network.
    const VALIDITY: u32 = (1 << 0) | (1 << 3) | (1 << 4) | (1 << 7) | (1 << 29);

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        fn SecStaticCodeCreateWithPath(path: Ref, flags: u32, code: *mut Ref) -> i32;
        fn SecCodeCopySelf(flags: u32, code: *mut Ref) -> i32;
        fn SecCodeCopySigningInformation(code: Ref, flags: u32, information: *mut Ref) -> i32;
        fn SecRequirementCreateWithString(text: Ref, flags: u32, requirement: *mut Ref) -> i32;
        fn SecStaticCodeCheckValidity(code: Ref, flags: u32, requirement: Ref) -> i32;
        static kSecCodeInfoTeamIdentifier: Ref;
        #[cfg(test)]
        static kSecCodeInfoIdentifier: Ref;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFURLCreateFromFileSystemRepresentation(allocator: Ref, buffer: *const u8, length: isize, directory: u8) -> Ref;
        fn CFStringCreateWithBytes(allocator: Ref, bytes: *const u8, length: isize, encoding: u32, external: u8) -> Ref;
        fn CFDictionaryGetValue(dictionary: Ref, key: Ref) -> Ref;
        fn CFGetTypeID(value: Ref) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFStringGetCString(string: Ref, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
        fn CFRelease(value: Ref);
    }

    /// A Core Foundation object this code created, released once.
    struct Owned(Ref);

    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: only non-null objects this module created are wrapped.
            unsafe { CFRelease(self.0) }
        }
    }

    fn owned(object: Ref) -> Option<Owned> {
        (!object.is_null()).then_some(Owned(object))
    }

    fn static_code(path: &Path) -> Option<Owned> {
        let bytes = path.as_os_str().as_bytes();
        // SAFETY: the buffer outlives the call; the URL and code are owned.
        unsafe {
            let url = owned(CFURLCreateFromFileSystemRepresentation(null(), bytes.as_ptr(), bytes.len() as isize, u8::from(path.is_dir())))?;
            let mut code = null();
            if SecStaticCodeCreateWithPath(url.0, 0, &mut code) != 0 {
                return None;
            }
            owned(code)
        }
    }

    fn team(code: Ref) -> Option<String> {
        // SAFETY: a constant the framework defines.
        info_text(code, unsafe { kSecCodeInfoTeamIdentifier })
    }

    /// The signing identifier, which ad-hoc signatures have too.
    #[cfg(test)]
    pub(super) fn identifier(path: &Path) -> Option<String> {
        // SAFETY: a constant the framework defines.
        info_text(static_code(path)?.0, unsafe { kSecCodeInfoIdentifier })
    }

    fn info_text(code: Ref, key: Ref) -> Option<String> {
        let mut buffer = [0 as c_char; 256];
        // SAFETY: the dictionary is owned; the value it holds is read, type
        // checked and copied into a buffer of the size given before release.
        unsafe {
            let mut information = null();
            if SecCodeCopySigningInformation(code, SIGNING_INFORMATION, &mut information) != 0 {
                return None;
            }
            let information = owned(information)?;
            let value = CFDictionaryGetValue(information.0, key);
            if value.is_null() || CFGetTypeID(value) != CFStringGetTypeID() || CFStringGetCString(value, buffer.as_mut_ptr(), buffer.len() as isize, UTF8) == 0 {
                return None;
            }
            Some(CStr::from_ptr(buffer.as_ptr()).to_string_lossy().into_owned()).filter(|text| !text.is_empty())
        }
    }

    pub(super) fn running_team() -> Option<String> {
        // SAFETY: the code object is owned and released.
        let code = unsafe {
            let mut code = null();
            if SecCodeCopySelf(0, &mut code) != 0 {
                return None;
            }
            owned(code)?
        };
        team(code.0)
    }

    pub(super) fn team_of(path: &Path) -> Option<String> {
        team(static_code(path)?.0)
    }

    pub(super) fn valid(path: &Path, requirement: Option<&str>) -> bool {
        let Some(code) = static_code(path) else { return false };
        // SAFETY: the text outlives the call; the string and requirement are owned.
        unsafe {
            let requirement = match requirement {
                None => None,
                Some(text) => {
                    let Some(text) = owned(CFStringCreateWithBytes(null(), text.as_ptr(), text.len() as isize, UTF8, 0)) else { return false };
                    let mut requirement = null();
                    if SecRequirementCreateWithString(text.0, 0, &mut requirement) != 0 {
                        return false;
                    }
                    let Some(requirement) = owned(requirement) else { return false };
                    Some(requirement)
                }
            };
            SecStaticCodeCheckValidity(code.0, VALIDITY, requirement.as_ref().map_or(null(), |r| r.0)) == 0
        }
    }
}

#[cfg(test)]
#[path = "codesign_tests.rs"]
mod tests;
