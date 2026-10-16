//! Registration hook used by the CSRF coverage test. Production uses Axum's
//! normal routing; tests capture the actual registered paths, including loops.
use axum::{routing::MethodRouter, Router};

pub(crate) trait RegisteredRoutes<S> {
    fn registered_route(self, path: &str, methods: MethodRouter<S>) -> Self;
}

impl<S: Clone + Send + Sync + 'static> RegisteredRoutes<S> for Router<S> {
    fn registered_route(self, path: &str, methods: MethodRouter<S>) -> Self {
        #[cfg(test)]
        PATHS.with_borrow_mut(|paths| {
            paths.push((path.to_owned(), crate::auth::cookie_origin_class(path)))
        });
        self.route(path, methods)
    }
}

#[cfg(test)]
thread_local! {
    static PATHS: std::cell::RefCell<Vec<(String, crate::auth::CookieOriginClass)>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
pub(super) fn take_paths() -> Vec<(String, crate::auth::CookieOriginClass)> {
    PATHS.with_borrow_mut(std::mem::take)
}
