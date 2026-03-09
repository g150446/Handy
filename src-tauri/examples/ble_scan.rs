//! Integration test: scan → connect → 3秒録音 → 切断
//! Run with:  cargo run --example ble_scan

use handy_app_lib::ble::BleManager;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    println!("=== Handy BLE テスト ===\n");

    let ble = Arc::new(BleManager::new());

    println!("[1] スキャン (5秒)...");
    let devices = ble.scan_devices(5).await.expect("scan failed");
    for d in &devices { println!("    発見: {}", d); }
    if devices.is_empty() { eprintln!("デバイスなし"); return; }

    println!("\n[2] connect_first で接続中...");
    ble.connect_first(5).await.expect("connect failed");
    let s = ble.status();
    println!("    接続完了: {:?} / {:?}", s.device_name, s.device_address);

    println!("\n[3] 録音開始 (0x01) → 3秒待機...");
    ble.start_recording_command().await.expect("start failed");
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    println!("[4] 録音停止 (0x00)...");
    let samples = ble.stop_recording_command().await.expect("stop failed");
    println!("    受信サンプル数: {} ({:.2}秒 @ 16kHz)", samples.len(), samples.len() as f32 / 16000.0);

    println!("\n[5] 切断...");
    ble.disconnect().await.expect("disconnect failed");
    println!("    完了");
}
