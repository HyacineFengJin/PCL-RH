pub(crate) mod announcements;
pub(crate) mod provider;
use crate::plugin_host::Registration;
pub use crate::product_plugins::contracts::{
    AnnouncementProvider, AnnouncementService, ANNOUNCEMENTS,
};
use announcements::{AnnouncementScope, AnnouncementStore, AnnouncementView};
use std::{future::Future, pin::Pin, sync::Arc};
pub const ID: &str = "launcher.home.announcements";

impl AnnouncementProvider for AnnouncementStore {
    fn read(
        &self,
        scope: AnnouncementScope,
        refresh: bool,
    ) -> Pin<Box<dyn Future<Output = AnnouncementView> + Send + '_>> {
        Box::pin(AnnouncementStore::read(self, scope, refresh))
    }
}
pub fn start(registration: &mut Registration<'_>) -> Result<(), String> {
    // The plugin owns the network cache and in-flight fetch synchronization.
    registration.provide(
        ANNOUNCEMENTS,
        AnnouncementService(Arc::new(AnnouncementStore::default())),
    )
}
