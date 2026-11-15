// Copyright © 2016-2026 The SENTIENT Authors

//! The Platform Manager's place in the Windows shell: one AppUserModelID
//! for the process, its Start menu shortcut and its toasts, so the taskbar
//! button's right-click Jump List and the notifications belong to it.
//!
//! The Jump List's Tasks are crate::background::ACTIONS, each a link to this
//! program with `--manager --action <name>` (handed to the running copy).

use std::path::Path;

use windows::Win32::Storage::EnhancedStorage::{PKEY_AppUserModel_ID, PKEY_Title};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, IPersistFile, STGM_READ, STGM_READWRITE};
use windows::Win32::UI::Shell::Common::{IObjectArray, IObjectCollection};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{DestinationList, EnumerableObjectCollection, ICustomDestinationList, IShellLinkW, SetCurrentProcessExplicitAppUserModelID, ShellLink};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::core::{HSTRING, Interface};

/// The Platform Manager's AppUserModelID.
pub const AUMID: &str = "INVENIA.SENTIENT.PlatformManager";

/// This process is the Platform Manager (its taskbar button, Jump List, toasts).
pub fn set_app_id() {
    unsafe {
        let _ = SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(AUMID));
    }
}

/// The Jump List's Tasks: the taskbar actions, each a link to this program.
pub fn update_jump_list() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let list: ICustomDestinationList = CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        list.SetAppID(&HSTRING::from(AUMID)).map_err(|e| e.to_string())?;
        let mut slots = 0u32;
        let _removed: IObjectArray = list.BeginList(&mut slots).map_err(|e| e.to_string())?;
        let tasks: IObjectCollection = CoCreateInstance(&EnumerableObjectCollection, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        for (action, label) in crate::background::ACTIONS.iter().filter(|(a, _)| *a != "open") {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
            link.SetPath(&HSTRING::from(exe.as_os_str())).map_err(|e| e.to_string())?;
            link.SetArguments(&HSTRING::from(format!("--manager --action {action}"))).map_err(|e| e.to_string())?;
            // Icon 2 of the program: the Platform Manager's (build.rs).
            link.SetIconLocation(&HSTRING::from(exe.as_os_str()), 1).map_err(|e| e.to_string())?;
            let store: IPropertyStore = link.cast().map_err(|e| e.to_string())?;
            store.SetValue(&PKEY_Title, &PROPVARIANT::from(*label)).map_err(|e| e.to_string())?;
            store.Commit().map_err(|e| e.to_string())?;
            tasks.AddObject(&link).map_err(|e| e.to_string())?;
        }
        let array: IObjectArray = tasks.cast().map_err(|e| e.to_string())?;
        list.AddUserTasks(&array).map_err(|e| e.to_string())?;
        list.CommitList().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Shortcuts from earlier builds have no AppUserModelID, so pinned they show
/// no Jump List: give each of our own that lacks it the id, once. Errors go
/// to the activity log.
pub fn stamp_own_shortcuts() {
    for lnk in crate::self_install::manager_links().into_iter().filter(|p| p.is_file()) {
        match shortcut_app_id(&lnk) {
            Ok(Some(id)) if id == AUMID => {}
            Ok(_) => {
                if let Err(e) = set_shortcut_app_id(&lnk) {
                    crate::maintenance_ops::log_line(&format!("The shortcut {} has no app id: {e}", lnk.display()));
                }
            }
            Err(e) => crate::maintenance_ops::log_line(&format!("The shortcut {} could not be read: {e}", lnk.display())),
        }
    }
}

/// A shortcut's AppUserModelID, if it has one.
fn shortcut_app_id(lnk: &Path) -> Result<Option<String>, String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        let file: IPersistFile = link.cast().map_err(|e| e.to_string())?;
        file.Load(&HSTRING::from(lnk.as_os_str()), STGM_READ).map_err(|e| e.to_string())?;
        let store: IPropertyStore = link.cast().map_err(|e| e.to_string())?;
        let v = store.GetValue(&PKEY_AppUserModel_ID).map_err(|e| e.to_string())?;
        let s = v.to_string();
        Ok((!s.is_empty()).then_some(s))
    }
}

/// Give a shortcut the Platform Manager's AppUserModelID, so a pinned or
/// Start menu shortcut shows its Jump List and owns its toasts.
pub fn set_shortcut_app_id(lnk: &Path) -> Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        let file: IPersistFile = link.cast().map_err(|e| e.to_string())?;
        let path = HSTRING::from(lnk.as_os_str());
        file.Load(&path, STGM_READWRITE).map_err(|e| e.to_string())?;
        let store: IPropertyStore = link.cast().map_err(|e| e.to_string())?;
        store.SetValue(&PKEY_AppUserModel_ID, &PROPVARIANT::from(AUMID)).map_err(|e| e.to_string())?;
        store.Commit().map_err(|e| e.to_string())?;
        file.Save(&path, true).map_err(|e| e.to_string())?;
    }
    Ok(())
}
