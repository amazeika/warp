use super::*;

#[test]
fn the_log_path_is_per_extension_under_its_root() {
    let root = std::path::Path::new("/var/warp/logs/extensions");
    assert_eq!(
        extension_log_path_in(root, "dev.warp.git"),
        root.join("dev.warp.git.log")
    );
}

#[test]
fn an_id_cannot_steer_the_log_out_of_its_directory() {
    let root = std::path::Path::new("/var/warp/logs/extensions");
    let path = extension_log_path_in(root, "../../etc/passwd");
    assert_eq!(path.parent(), Some(root));
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some(".._.._etc_passwd.log")
    );
}

#[test]
fn ordinary_lines_are_written_verbatim() {
    let mut buffer = Vec::new();
    let mut writer = RedactingWriter::new(&mut buffer);
    writer
        .write_line("started warp-git 0.1.0")
        .expect("line writes");
    assert_eq!(
        String::from_utf8(buffer).expect("utf8"),
        "started warp-git 0.1.0\n"
    );
}

#[test]
fn lines_naming_a_secret_are_dropped_whole() {
    for line in [
        "Authorization: Bearer abc123",
        "GIT_ASKPASS password=hunter2",
        "api_key=sk-live-1234",
        "PRIVATE_KEY=-----BEGIN",
    ] {
        assert!(
            RedactingWriter::<Vec<u8>>::is_sensitive(line),
            "`{line}` must be treated as sensitive"
        );
        let mut buffer = Vec::new();
        RedactingWriter::new(&mut buffer)
            .write_line(line)
            .expect("line writes");
        let written = String::from_utf8(buffer).expect("utf8");
        assert!(!written.contains("hunter2"));
        assert!(!written.contains("abc123"));
        assert!(!written.contains("sk-live-1234"));
        assert!(written.contains("redacted"));
    }
}
