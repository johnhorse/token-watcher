// Windows release 下不弹控制台窗口，勿删
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    token_watcher_lib::run()
}
