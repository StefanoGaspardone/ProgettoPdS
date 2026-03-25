mod common;

use common::TestEnvironment;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write, Seek, SeekFrom};

#[test]
fn client_files_put_get_and_range_read_work() {
    let env = TestEnvironment::new();
    let file_path = env.mount_point.join("notes.txt");

    fs::write(&file_path, "hello world").expect("Impossibile scrivere il file");

    let content = fs::read_to_string(&file_path).expect("Impossibile leggere il file");
    assert_eq!(content, "hello world");

    let mut file = fs::File::open(&file_path).expect("Impossibile aprire il file per il range read");
    file.seek(SeekFrom::Start(6)).expect("Impossibile fare il seek");
    
    let mut buffer = vec![0; 5];
    file.read_exact(&mut buffer).expect("Impossibile leggere il range");
    assert_eq!(String::from_utf8(buffer).unwrap(), "world");
}

#[test]
fn client_files_offset_write_and_edge_reads_work() {
    let env = TestEnvironment::new();
    let file_path = env.mount_point.join("edge.txt");

    fs::write(&file_path, "abcdef").expect("Impossibile creare il file edge");

    let mut file = OpenOptions::new()
        .write(true)
        .open(&file_path)
        .expect("Impossibile aprire il file in scrittura");
    
    file.seek(SeekFrom::Start(2)).expect("Impossibile fare il seek per la scrittura");
    file.write_all(b"ZZ").expect("Impossibile sovrascrivere i byte");
    drop(file);

    let patched_content = fs::read_to_string(&file_path).expect("Impossibile leggere il file patchato");
    assert_eq!(patched_content, "abZZef");

    let mut file_read = fs::File::open(&file_path).unwrap();
    let mut zero_buf = [0u8; 0];
    let bytes_read_zero = file_read.read(&mut zero_buf).unwrap();
    assert_eq!(bytes_read_zero, 0, "Una lettura di 0 byte deve restituire 0");

    file_read.seek(SeekFrom::Start(1000)).unwrap();
    let mut empty_buf = Vec::new();
    let bytes_read_eof = file_read.read_to_end(&mut empty_buf).unwrap();
    assert_eq!(bytes_read_eof, 0, "Una lettura oltre l'EOF deve restituire 0 byte");
}

#[test]
fn client_files_delete_file_and_directory_work() {
    let env = TestEnvironment::new();
    let file_path = env.mount_point.join("temp.txt");

    fs::write(&file_path, "abc").expect("Impossibile creare file temp");

    fs::remove_file(&file_path).expect("Impossibile eliminare il file");

    let read_result = fs::read_to_string(&file_path);
    assert!(read_result.is_err(), "Il file eliminato non dovrebbe poter essere letto");

    let tree_path = env.mount_point.join("tree");
    let sub_path = tree_path.join("sub");
    fs::create_dir_all(&sub_path).expect("Impossibile creare tree/sub");
    
    let leaf_path = sub_path.join("leaf.txt");
    fs::write(&leaf_path, "leaf").expect("Impossibile scrivere leaf.txt");

    fs::remove_dir_all(&tree_path).expect("Impossibile eliminare l'albero delle cartelle");
    assert!(!tree_path.exists(), "La cartella tree dovrebbe essere stata eliminata");
}

#[test]
fn client_files_error_cases_handled_correctly() {
    let env = TestEnvironment::new();

    let missing_file = env.mount_point.join("missing.txt");
    let missing_read = fs::read_to_string(&missing_file);
    assert!(missing_read.is_err(), "Leggere un file mancante deve fallire");

    let missing_delete = fs::remove_file(&missing_file);
    assert!(missing_delete.is_err(), "Eliminare un file mancante deve fallire");

    let missing_parent_file = env.mount_point.join("missing_parent").join("file.txt");
    let missing_parent_write = fs::write(&missing_parent_file, "x");
    assert!(missing_parent_write.is_err(), "Scrivere in un percorso senza padre deve fallire");
}