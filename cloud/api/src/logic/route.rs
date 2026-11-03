//! The API's routes, and which of them act for a signed-in account.

/// A request the Worker answers. Anything else is `404 not_found`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route<'a> {
    Health,
    AuthStart,
    AuthVerify,
    /// `POST /v1/auth/clerk`: a browser sign-in, without a session.
    AuthClerk,
    /// `POST /v1/auth/clerk/link`: a browser sign-in to link to the caller's account.
    ClerkLink,
    /// `POST /v1/auth/clerk/link/confirm`
    ClerkLinkConfirm,
    Logout,
    Me,
    RevokeDevice(&'a str),
    SyncPush,
    SyncPull,
    SyncSocket,
    /// `DELETE /v1/sync/data`: the account's sync data, not the account.
    SyncDelete,
    /// `DELETE /v1/account`: the account itself, after a step-up.
    DeleteAccount,
    Ai,
}

impl<'a> Route<'a> {
    pub fn parse(method: &str, path: &'a str) -> Option<Route<'a>> {
        let segments: Vec<&'a str> = path.trim_matches('/').split('/').collect();
        Some(match (method, segments.as_slice()) {
            ("GET", ["v1", "health"]) => Route::Health,
            ("POST", ["v1", "auth", "start"]) => Route::AuthStart,
            ("POST", ["v1", "auth", "verify"]) => Route::AuthVerify,
            ("POST", ["v1", "auth", "clerk"]) => Route::AuthClerk,
            ("POST", ["v1", "auth", "clerk", "link"]) => Route::ClerkLink,
            ("POST", ["v1", "auth", "clerk", "link", "confirm"]) => Route::ClerkLinkConfirm,
            ("POST", ["v1", "auth", "logout"]) => Route::Logout,
            ("GET", ["v1", "me"]) => Route::Me,
            ("DELETE", ["v1", "devices", id]) => Route::RevokeDevice(id),
            ("POST", ["v1", "sync", "push"]) => Route::SyncPush,
            ("GET", ["v1", "sync", "pull"]) => Route::SyncPull,
            ("GET", ["v1", "sync", "ws"]) => Route::SyncSocket,
            ("DELETE", ["v1", "sync", "data"]) => Route::SyncDelete,
            ("DELETE", ["v1", "account"]) => Route::DeleteAccount,
            (_, ["v1", "ai", ..]) => Route::Ai,
            _ => return None,
        })
    }

    /// Every route but health and the sign-in steps is answered only after
    /// the bearer session is checked. Linking a browser sign-in needs one.
    pub fn needs_session(&self) -> bool {
        !matches!(self, Route::Health | Route::AuthStart | Route::AuthVerify | Route::AuthClerk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_health_and_the_sign_in_steps_answer_without_a_session() {
        let public = [("GET", "/v1/health"), ("POST", "/v1/auth/start"), ("POST", "/v1/auth/verify"), ("POST", "/v1/auth/clerk")];
        for (method, path) in public {
            assert!(!Route::parse(method, path).unwrap().needs_session(), "{method} {path}");
        }
        let private = [
            ("POST", "/v1/auth/logout"), ("GET", "/v1/me"), ("DELETE", "/v1/devices/d1"),
            ("POST", "/v1/sync/push"), ("GET", "/v1/sync/pull"), ("GET", "/v1/sync/ws"),
            ("DELETE", "/v1/sync/data"), ("POST", "/v1/ai/complete"), ("GET", "/v1/ai/health"),
            ("POST", "/v1/auth/clerk/link"), ("POST", "/v1/auth/clerk/link/confirm"),
            ("DELETE", "/v1/account"),
        ];
        for (method, path) in private {
            assert!(Route::parse(method, path).unwrap().needs_session(), "{method} {path}");
        }
    }

    #[test]
    fn deleting_the_account_is_one_exact_route() {
        assert_eq!(Route::parse("DELETE", "/v1/account"), Some(Route::DeleteAccount));
        for (method, path) in [("GET", "/v1/account"), ("POST", "/v1/account"), ("DELETE", "/v1/account/x"), ("DELETE", "/v1/accounts"), ("DELETE", "/v1")] {
            assert_eq!(Route::parse(method, path), None, "{method} {path}");
        }
    }

    #[test]
    fn deleting_sync_data_is_one_exact_route() {
        assert_eq!(Route::parse("DELETE", "/v1/sync/data"), Some(Route::SyncDelete));
        assert_eq!(Route::parse("DELETE", "/v1/devices/d1"), Some(Route::RevokeDevice("d1")));
        for (method, path) in [("GET", "/v1/sync/data"), ("POST", "/v1/sync/data"), ("DELETE", "/v1/sync"), ("DELETE", "/v1/sync/data/all"), ("GET", "/v1/nope")] {
            assert_eq!(Route::parse(method, path), None, "{method} {path}");
        }
    }
}
