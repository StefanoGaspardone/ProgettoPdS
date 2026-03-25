mod common;

use common::TestEnvironment;
use std::fs;

#[test]
fn client_list_root_and_nested_directory_work() {
    let env = TestEnvironment::new();

    let nested_dir = env.mount_point.join("a").join("b");
    fs::create_dir_all(&nested_dir).expect("Fallita creazione cartelle annidate");

    let file_path = nested_dir.join("item.txt");
    fs::write(&file_path, "data").expect("Fallita scrittura file");

    let mut root_entries = fs::read_dir(&env.mount_point)
        .expect("Impossibile leggere la cartella root")
        .filter_map(Result::ok);
    
    assert!(
        root_entries.any(|e| e.file_name() == "a" && e.file_type().unwrap().is_dir()),
        "La root dovrebbe contenere la directory 'a'"
    );

    let mut nested_entries = fs::read_dir(&nested_dir)
        .expect("Impossibile leggere la cartella annidata")
        .filter_map(Result::ok);
    
    assert!(
        nested_entries.any(|e| e.file_name() == "item.txt" && e.file_type().unwrap().is_file()),
        "La cartella annidata dovrebbe contenere il file 'item.txt'"
    );
}

#[test]
fn client_list_missing_directory_behavior() {
    let env = TestEnvironment::new();
    let missing_dir = env.mount_point.join("missing-dir");
    
    let result = fs::read_dir(&missing_dir);
    
    assert!(
        result.is_err(), 
        "Cercare di elencare una cartella inesistente tramite OS deve restituire errore"
    );


}