//! The Anthropic API key that Claude chats on API key billing get, as the windows see it:
//! where it is kept (`riwork_engine::anthropic_key`, the Keychain) and whether one is
//! saved, as the store last said. The key itself is never kept here: Settings hands a
//! paste to the store and clears its field, and the chat host reads the key when a chat
//! starts.

use gpui::{App, Global};
use riwork_engine::anthropic_key::{self, KeyStore};
use std::sync::Arc;

#[derive(Clone)]
pub struct ApiKeyState {
    store: Arc<dyn KeyStore>,
    /// Whether a key is saved, as the store last answered; `None` until it is asked.
    saved: Option<Result<bool, String>>,
}

impl Global for ApiKeyState {}

impl Default for ApiKeyState {
    /// The login Keychain. A test build's refuses every call (`anthropic_key::Keychain`).
    fn default() -> Self {
        Self::with_store(Arc::new(anthropic_key::Keychain))
    }
}

impl ApiKeyState {
    pub fn with_store(store: Arc<dyn KeyStore>) -> Self {
        Self { store, saved: None }
    }
}

/// The store the windows use.
pub fn store(cx: &App) -> Arc<dyn KeyStore> {
    cx.try_global::<ApiKeyState>()
        .map_or_else(|| ApiKeyState::default().store, |state| state.store.clone())
}

/// Whether a key is saved: the last answer, or the store's own if it was never asked. The
/// store is asked for the item's attributes only, never the key.
pub fn status(cx: &mut App) -> Result<bool, String> {
    if let Some(saved) = cx
        .try_global::<ApiKeyState>()
        .and_then(|state| state.saved.clone())
    {
        return saved;
    }
    refresh(cx)
}

/// Whether a key is saved, a store that cannot say counting as none.
pub fn saved(cx: &mut App) -> bool {
    status(cx).unwrap_or(false)
}

/// Asks the store again, as Settings does when it is shown: the key may have been removed
/// in Keychain Access.
pub fn refresh(cx: &mut App) -> Result<bool, String> {
    let store = store(cx);
    let saved = store.is_saved();
    remember(cx, store, saved.clone());
    saved
}

/// What a save or a removal left: the windows redraw what depends on it.
pub fn remember(cx: &mut App, store: Arc<dyn KeyStore>, saved: Result<bool, String>) {
    let changed = cx
        .try_global::<ApiKeyState>()
        .is_none_or(|state| state.saved.as_ref() != Some(&saved));
    if changed {
        cx.set_global(ApiKeyState {
            store,
            saved: Some(saved),
        });
    }
}
