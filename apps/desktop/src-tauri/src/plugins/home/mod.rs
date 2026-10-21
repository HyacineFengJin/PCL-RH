pub(crate) mod announcements;
pub(crate) mod provider;
use crate::plugin_host::{Contract, Registration};
use announcements::{AnnouncementScope, AnnouncementStore, AnnouncementView};
use std::{future::Future, pin::Pin, sync::Arc};

pub const ID: &str = "launcher.home.announcements";
pub const ANNOUNCEMENTS: Contract<AnnouncementService> = Contract::new("launcher.announcements", 1);

pub trait AnnouncementProvider: Send + Sync {
    fn read(
        &self,
        scope: AnnouncementScope,
        refresh: bool,
    ) -> Pin<Box<dyn Future<Output = AnnouncementView> + Send + '_>>;
}
impl AnnouncementProvider for AnnouncementStore {
    fn read(
        &self,
        scope: AnnouncementScope,
        refresh: bool,
    ) -> Pin<Box<dyn Future<Output = AnnouncementView> + Send + '_>> {
        Box::pin(AnnouncementStore::read(self, scope, refresh))
    }
}
pub struct AnnouncementService(pub Arc<dyn AnnouncementProvider>);
pub fn start(registration: &mut Registration<'_>) -> Result<(), String> {
    // The plugin owns the network cache and in-flight fetch synchronization.
    registration.provide(
        ANNOUNCEMENTS,
        AnnouncementService(Arc::new(AnnouncementStore::default())),
    )
}
