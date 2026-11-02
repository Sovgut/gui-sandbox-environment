//! The time the app shows things against: now, or the fixed moment a
//! preview happens at.

/// Now, as the app compares times with it.
pub fn now() -> chrono::DateTime<chrono::Local> {
    if crate::preview::active() {
        crate::preview::now()
    } else {
        chrono::Local::now()
    }
}
