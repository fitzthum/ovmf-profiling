// Benchmarking tool for OVMF

use std::process::{Command,Stdio};
use std::io::{BufRead, BufReader};
use std::os::unix::net::{UnixStream,UnixListener};
use std::{thread, time};
use std::sync::{Arc, Mutex};
use std::collections::HashMap;

const HV_PATH: &str = "/home/tobin/qemu/build/qemu-system-x86_64";
const KERNEL_PATH: &str = "/opt/kata/share/kata-containers/vmlinuz-confidential.container";
const INITRD_PATH: &str = "/home/tobin/kata-containers/tools/osbuilder/initrd-builder/kata-containers-initrd.img";
const FW_PATH: &str = "/home/tobin/edk2/Build/OvmfX64/DEBUG_GCC5/FV/OVMF.fd";

const DEBUG_SOCKET: &str = "/tmp/ovmf_output.sock";
fn main() {
    start_guest(true);
}

fn start_guest(sev_enabled: bool) {
    let mut cmd = Command::new("sudo");

    cmd.arg(HV_PATH);

    if sev_enabled {
        cmd.args(["-name","direct-snp"]);
        cmd.args(["-machine","q35,accel=kvm,kernel_irqchip=split,confidential-guest-support=sev0"]);
        cmd.args(["-object","sev-snp-guest,id=sev0,policy=0x30000,kernel-hashes=off,reduced-phys-bits=5,cbitpos=51"]);
    }
    else {
        cmd.args(["-name","direct-nosev"]);
        cmd.args(["-machine","q35,accel=kvm,kernel_irqchip=split"]);
    }

    // basic guest properties
    cmd.arg("-enable-kvm");
    cmd.args(["-cpu","EPYC-v4"]);
    cmd.args(["-smp","2"]);
    cmd.args(["-m","512"]);

    // artifacts
    cmd.args(["-initrd",INITRD_PATH]);
    cmd.args(["-kernel",KERNEL_PATH]);
    cmd.args(["-append","\"console=ttyS0\""]);

    let fw_drive = format!("if=pflash,format=raw,readonly=on,file={}",FW_PATH);
    cmd.args(["-drive",&fw_drive]);

    // devices
    cmd.arg("-nographic");
    cmd.args(["-device","virtio-scsi-pci,id=scsi,disable-legacy=on,iommu_platform=true"]);
    cmd.args(["-chardev","file,id=char0,path=serial-output.txt"]);
    cmd.args(["-serial","chardev:char0"]);

    let debug_dev = format!("socket,path={},id=fwdbg", DEBUG_SOCKET);
    cmd.args(["-chardev", &debug_dev]);
    cmd.args(["-device", "isa-debugcon,iobase=0x402,chardev=fwdbg"]);

    cmd.stdout(Stdio::null());

    let debug_log = Arc::new(Mutex::new(Vec::new()));

    std::fs::remove_file(DEBUG_SOCKET);
    let listener = UnixListener::bind(DEBUG_SOCKET).unwrap();

    println!("Starting Guest");
    let mut child = cmd.spawn().unwrap();

    let stream = listener.incoming().next().unwrap();
    let debug_log_clone = debug_log.clone();
    let handler = thread::spawn(move || handle_debug(stream.unwrap(), debug_log_clone));

	let ten_seconds = time::Duration::from_secs(10);
	thread::sleep(ten_seconds);

	child.kill();
    handler.join().unwrap();

    /*
    for line in debug_log.lock().unwrap().iter() {
        println!("{}",  line.0);
    }
    */

    // the log entries that define each phase
    let keypoints = vec![
        "SecCoreStartupWithStack",
        "Platform PEIM Loaded",
        "MpInitChangeApLoopCallback() done!",
    ];

    // get the times just for the keypoints
    let mut keypoint_times = HashMap::new();

    for (message, timestamp) in debug_log.lock().unwrap().iter() {
        for keypoint in &keypoints {
            if message.contains(keypoint) {
                keypoint_times.insert(keypoint, timestamp);
				print!("{} - {}\n", timestamp, keypoint);
                break;
            }
        }
    }
}

fn handle_debug(stream: UnixStream, debug_log: Arc<Mutex<Vec<(String, u128)>>>) {
	let stream = BufReader::new(stream);

    let now = time::Instant::now();

	for line in stream.lines() {
        let elapsed = now.elapsed().as_millis();
        debug_log.lock().unwrap().push((line.unwrap(), elapsed));
	}

}
