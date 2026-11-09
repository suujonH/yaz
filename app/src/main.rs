#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod gallery;
mod pages;
mod platform;
#[cfg(debug_assertions)]
mod request_log;
mod root;
mod services;
mod shell;
mod startup;
mod state;
mod ui;
mod widgets;
mod windows;

use dioxus::prelude::*;
use shell::{AppShared, WindowKey, local_app_data, window_config};
use std::sync::Arc;
use zulip_store::DataLocation;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    startup::wait_for_previous_instance(&args);

    let location = DataLocation::at(local_app_data().join(DataLocation::FOLDER_NAME));
    let update = services::create_update_service(&location);
    if update
        .as_ref()
        .is_some_and(|update| update.try_apply_staged(&args))
    {
        return;
    }
    if !startup::acquire_instance(location.default_root()) {
        return;
    }
    services::set_boot(startup::Boot {
        args,
        location,
        update,
    });

    let gallery = std::env::var("ZULIP_GALLERY").is_ok_and(|v| v == "1");
    // 组件预览不走启动序列，直接用解析出的目录
    let problem = services::check_data_location().filter(|_| !gallery);
    let stage = match problem {
        Some(problem) => root::Stage::DataProblem(problem),
        None => {
            let moving = services::boot().location.pending_move();
            if moving.is_none() && !services::init_or_report() {
                return;
            }
            root::initial_stage(moving)
        }
    };

    let shared = AppShared {
        theme: services::create_theme(),
        bounds: Arc::new(services::SettingsBounds),
    };
    let spec = root::root_window_spec(&stage);
    let config = window_config(&spec, &shared);
    LaunchBuilder::desktop()
        .with_cfg(config)
        .with_context(shared.clone())
        .with_context(shared.theme.clone())
        .with_context(WindowKey {
            bounds_key: spec.bounds_key.clone(),
            remember: spec.remember_bounds,
        })
        .with_context(stage)
        .launch(if gallery {
            gallery::Gallery
        } else {
            root::Root
        });
}
