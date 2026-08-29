use coop_attestation::{
    encode_private_key_pem, read_private_key_file, read_public_key_file,
    write_private_key_file_new, write_public_key_file_new, AttestationError, SigningKey,
    MAX_KEY_FILE_BYTES,
};
use std::fs;
use std::io::Write;

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[42_u8; 32])
}

#[test]
fn key_files_round_trip_and_never_overwrite() {
    let temp = tempfile::tempdir().unwrap();
    let private_path = temp.path().join("signing-key.pem");
    let public_path = temp.path().join("signing-key.pub.pem");
    let signing_key = signing_key();

    write_private_key_file_new(&private_path, &signing_key).unwrap();
    write_public_key_file_new(&public_path, &signing_key.verifying_key()).unwrap();
    assert_eq!(
        read_private_key_file(&private_path).unwrap().to_bytes(),
        signing_key.to_bytes()
    );
    assert_eq!(
        read_public_key_file(&public_path).unwrap(),
        signing_key.verifying_key()
    );

    assert!(matches!(
        write_private_key_file_new(&private_path, &signing_key),
        Err(AttestationError::KeyFileAlreadyExists)
    ));
    assert!(matches!(
        write_public_key_file_new(&public_path, &signing_key.verifying_key()),
        Err(AttestationError::KeyFileAlreadyExists)
    ));
}

#[test]
fn private_key_file_rejects_noncanonical_and_oversized_input() {
    let temp = tempfile::tempdir().unwrap();
    let noncanonical_path = temp.path().join("noncanonical.pem");
    let oversized_path = temp.path().join("oversized.pem");
    let pem = encode_private_key_pem(&signing_key()).unwrap();
    fs::write(&noncanonical_path, pem.replace('\n', "\r\n")).unwrap();

    #[cfg(unix)]
    make_private_mode(&noncanonical_path);
    assert!(matches!(
        read_private_key_file(&noncanonical_path),
        Err(AttestationError::InvalidKeyEncoding { .. })
    ));

    let mut oversized = fs::File::create(&oversized_path).unwrap();
    oversized
        .write_all(&vec![b'x'; MAX_KEY_FILE_BYTES + 1])
        .unwrap();
    drop(oversized);
    #[cfg(unix)]
    make_private_mode(&oversized_path);
    assert!(matches!(
        read_private_key_file(&oversized_path),
        Err(AttestationError::KeyFileTooLarge { .. })
    ));
}

#[cfg(unix)]
fn make_private_mode(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};

    #[test]
    fn writers_use_expected_modes() {
        let temp = tempfile::tempdir().unwrap();
        let private_path = temp.path().join("private.pem");
        let public_path = temp.path().join("public.pem");
        let key = signing_key();
        write_private_key_file_new(&private_path, &key).unwrap();
        write_public_key_file_new(&public_path, &key.verifying_key()).unwrap();
        assert_eq!(fs::metadata(private_path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(public_path).unwrap().mode() & 0o777, 0o644);
    }

    #[test]
    fn readers_reject_unsafe_modes_symlinks_and_directories() {
        let temp = tempfile::tempdir().unwrap();
        let private_path = temp.path().join("private.pem");
        let public_path = temp.path().join("public.pem");
        let private_link = temp.path().join("private-link.pem");
        let key = signing_key();
        write_private_key_file_new(&private_path, &key).unwrap();
        write_public_key_file_new(&public_path, &key.verifying_key()).unwrap();

        fs::set_permissions(&private_path, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(matches!(
            read_private_key_file(&private_path),
            Err(AttestationError::UnsafeKeyFilePermissions { .. })
        ));
        fs::set_permissions(&private_path, fs::Permissions::from_mode(0o600)).unwrap();

        fs::set_permissions(&public_path, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(matches!(
            read_public_key_file(&public_path),
            Err(AttestationError::UnsafeKeyFilePermissions { .. })
        ));

        symlink(&private_path, &private_link).unwrap();
        assert!(matches!(
            read_private_key_file(&private_link),
            Err(AttestationError::UnsafeKeyFileType { .. })
        ));
        assert!(matches!(
            read_private_key_file(temp.path()),
            Err(AttestationError::UnsafeKeyFileType { .. })
        ));
    }

    #[test]
    fn writers_reject_externally_writable_non_sticky_ancestors() {
        let temp = tempfile::tempdir().unwrap();
        let unsafe_parent = temp.path().join("unsafe-parent");
        fs::create_dir(&unsafe_parent).unwrap();
        fs::set_permissions(&unsafe_parent, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(matches!(
            write_private_key_file_new(unsafe_parent.join("key.pem"), &signing_key()),
            Err(AttestationError::UnsafeKeyFilePermissions { .. })
        ));
    }

    #[test]
    fn non_root_owned_ancestor_symlinks_are_rejected() {
        if rustix::process::geteuid().as_raw() == 0 {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let real_parent = temp.path().join("real-parent");
        let alias = temp.path().join("alias");
        fs::create_dir(&real_parent).unwrap();
        symlink(&real_parent, &alias).unwrap();
        assert!(matches!(
            write_private_key_file_new(alias.join("key.pem"), &signing_key()),
            Err(AttestationError::UnsafeKeyFileOwner { .. })
        ));
    }
}
