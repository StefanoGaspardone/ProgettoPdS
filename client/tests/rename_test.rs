mod common;

use common::TestEnvironment;
use std::fs;

#[test]
fn client_rename_success_cases_work() {
    let env = TestEnvironment::new();

    let old_path = env.mount_point.join("rename-me.txt");
    let new_dir_path = env.mount_point.join("renamed");
    let new_path = new_dir_path.join("final.txt");

    fs::write(&old_path, "rename-content").expect("Impossibile creare il file originale");
    
    fs::create_dir(&new_dir_path).expect("Impossibile creare la cartella di destinazione");

    fs::rename(&old_path, &new_path).expect("Operazione di rinomina fallita");

    let renamed_content = fs::read_to_string(&new_path)
        .expect("Impossibile leggere il file dal nuovo percorso");
    assert_eq!(renamed_content, "rename-content");

    assert!(
        !old_path.exists(),
        "Il file originale esiste ancora dopo la rinomina!"
    );
}

#[test]
fn client_rename_error_cases_handled_correctly() {
    let env = TestEnvironment::new();

    
    let missing_old_path = env.mount_point.join("never-existed.txt");
    let target_path = env.mount_point.join("whatever.txt");

    
    let result = fs::rename(&missing_old_path, &target_path);

    assert!(
        result.is_err(),
        "Rinominare un file inesistente doveva restituire un errore"
    );
}