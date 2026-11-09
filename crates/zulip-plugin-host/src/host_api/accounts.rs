//! 账号：列出、选择与线程绑定。

use zulip_plugin_sdk::abi::{self, AccountCb, Ctx, UserData};

use super::{Fail, guard, run32};
use crate::marshal::{Scope, account_to_raw};
use crate::thread;

pub(super) unsafe extern "C" fn get_accounts(
    ctx: Ctx,
    cb: Option<AccountCb>,
    user: UserData,
) -> i32 {
    run32(
        ctx,
        "get_accounts",
        None,
        Box::new(move |call| {
            let cb = cb.ok_or(Fail::Code(abi::ERR_INVALID_ARG))?;
            let sessions = call.host.sessions();
            for session in &sessions {
                let mut scope = Scope::default();
                let raw = account_to_raw(session, &mut scope);
                // SAFETY: raw 在 scope 存活期间有效。
                unsafe { cb(&raw, user) };
            }
            Ok(sessions.len() as i64)
        }),
    )
}

pub(super) unsafe extern "C" fn get_current_account(ctx: Ctx) -> i64 {
    guard(
        ctx,
        "get_current_account",
        None,
        Box::new(|call| Ok(call.session()?.account_id())),
    )
}

pub(super) unsafe extern "C" fn select_account(ctx: Ctx, account_id: i64) -> i32 {
    run32(
        ctx,
        "select_account",
        None,
        Box::new(move |call| {
            let sessions = call.host.sessions();
            let Some(target) = sessions.iter().find(|s| s.account_id() == account_id) else {
                return Err(Fail::Code(abi::ERR_NOT_FOUND));
            };
            call.plugin.select_account(account_id);
            if call.host.is_plugin_thread() {
                thread::replace_call_session(target);
            }
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn set_thread_account(ctx: Ctx, account_id: i64) -> i32 {
    run32(
        ctx,
        "set_thread_account",
        None,
        Box::new(move |call| {
            if account_id == 0 {
                thread::bind_account(call.plugin, None);
                return Ok(0);
            }
            if !call
                .host
                .sessions()
                .iter()
                .any(|s| s.account_id() == account_id)
            {
                return Err(Fail::Code(abi::ERR_NOT_FOUND));
            }
            thread::bind_account(call.plugin, Some(account_id));
            Ok(0)
        }),
    )
}
