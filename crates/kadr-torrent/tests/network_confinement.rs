//! A dedicated test process: unrelated tests cannot add/remove network sockets
//! during the descriptor inspection. Linux-only evidence, not no-egress tracing.
#![cfg(target_os = "linux")]

use std::{mem, os::fd::RawFd, time::Duration};

use kadr_torrent::LocalSession;

fn socket_option(fd: RawFd, name: libc::c_int) -> Option<libc::c_int> {
    let mut value: libc::c_int = 0;
    let mut len = mem::size_of_val(&value) as libc::socklen_t;
    // SAFETY: getsockopt only borrows fd and writes within the supplied, sized
    // integer. Non-socket/vanished descriptors are rejected by the kernel.
    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            name,
            (&mut value as *mut libc::c_int).cast(),
            &mut len,
        )
    };
    (result == 0 && len as usize == mem::size_of_val(&value)).then_some(value)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_initial_tcp_and_udp_sockets_are_bound_to_loopback_device() {
    let directory = tempfile::tempdir().unwrap();
    let session = tokio::time::timeout(
        Duration::from_secs(5),
        LocalSession::new(directory.path().into()),
    )
    .await
    .unwrap()
    .unwrap();
    let mut tcp = 0;
    let mut udp = 0;
    for entry in std::fs::read_dir("/proc/self/fd").unwrap() {
        let entry = entry.unwrap();
        let Ok(fd) = entry.file_name().to_string_lossy().parse::<RawFd>() else {
            continue;
        };
        if !matches!(
            socket_option(fd, libc::SO_DOMAIN),
            Some(libc::AF_INET | libc::AF_INET6)
        ) {
            continue;
        }
        match socket_option(fd, libc::SO_TYPE) {
            Some(libc::SOCK_STREAM) => tcp += 1,
            Some(libc::SOCK_DGRAM) => udp += 1,
            _ => continue,
        }
        let mut device = [0_u8; libc::IFNAMSIZ];
        let mut len = device.len() as libc::socklen_t;
        // SAFETY: fd is borrowed, not owned/closed; buffer and length cover all
        // bytes the kernel may write. No pointer outlives this call.
        let result = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_BINDTODEVICE,
                device.as_mut_ptr().cast(),
                &mut len,
            )
        };
        assert_eq!(
            result,
            0,
            "query network socket interface: {}",
            std::io::Error::last_os_error()
        );
        assert_eq!(
            &device[..len as usize],
            b"lo\0",
            "unconfined network socket"
        );
    }
    assert!(tcp > 0, "must inspect the actual torrent listener");
    assert!(
        udp > 0,
        "must inspect upstream's always-created UDP tracker socket"
    );
    tokio::time::timeout(Duration::from_secs(3), session.stop())
        .await
        .unwrap();
}
