//! Pages of the selected account: shown when the account is online, and rebuilt as a whole after
//! the session changes (re-login).

use super::{AccountPage, Scrolled};
use crate::widgets::SessionRef;
use dioxus::prelude::*;

#[component]
pub fn AccountView(session: SessionRef, page: AccountPage) -> Element {
    super::use_store(Some(session.0.clone()), Some(session.0.account_id()));
    rsx! {
        div { class: "settings-main",
            Scrolled {
                match page {
                    AccountPage::Server => rsx! { super::server::Server {} },
                    AccountPage::Me => rsx! { super::me::Me {} },
                    AccountPage::Preferences => rsx! { super::preferences::Preferences {} },
                    AccountPage::Notifications => rsx! { super::preferences::Notifications {} },
                    AccountPage::EnhancedPush => rsx! { super::push::EnhancedPush {} },
                    AccountPage::GroupMessages => rsx! { super::levels::GroupMessages {} },
                    AccountPage::Privacy => rsx! { super::preferences::Privacy {} },
                    AccountPage::AlertWords => rsx! { super::preferences::AlertWords {} },
                    AccountPage::Resources => rsx! { super::resources::Resources {} },
                    AccountPage::Organization => rsx! { super::organization::Organization {} },
                }
            }
        }
    }
}
