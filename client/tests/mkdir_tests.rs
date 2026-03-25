mod common;

use common::TestEnvironment;
use std::fs;

#[test]
fn client_mkdir_create_and_idempotency_work() {
    let env = TestEnvironment::new();

    let nested_path = env.mount_point.join("same").join("dir");
    fs::create_dir_all(&nested_path).expect("Impossibile creare cartelle annidate");

    let metadata = fs::metadata(&nested_path).expect("Impossibile leggere i metadati");
    assert!(metadata.is_dir());


    let create_again_result = fs::create_dir_all(&nested_path);
    assert!(
        create_again_result.is_ok(),
        "create_dir_all dovrebbe essere idempotente e avere successo anche se la cartella esiste già"
    );
}

#[test]
fn client_mkdir_error_cases_handled_correctly() {
    let env = TestEnvironment::new();

    

    let dir_path = env.mount_point.join("duplicate");
    fs::create_dir(&dir_path).expect("Creazione iniziale fallita");

    let result = fs::create_dir(&dir_path);
    
    assert!(
        result.is_err(),
        "La creazione singola di una cartella già esistente deve restituire un errore a livello di file system"
    );
}