mod common;

use common::TestEnvironment;
use std::fs;

#[test]
fn client_stat_root_and_file_work() {
    let env = TestEnvironment::new();

    let file_path = env.mount_point.join("stat-file.txt");
    fs::write(&file_path, "12345").expect("Impossibile creare il file per il test stat");

    
    let root_meta = fs::metadata(&env.mount_point).expect("Impossibile leggere i metadati della root");
    assert!(root_meta.is_dir(), "La root del mount point non è riconosciuta come cartella");

    let file_meta = fs::metadata(&file_path).expect("Impossibile leggere i metadati del file");
    
    assert!(file_meta.is_file(), "Il sistema non riconosce l'elemento come file");
    assert_eq!(
        file_meta.len(),
        5,
        "La dimensione del file riportata dai metadati non corrisponde ai byte scritti"
    );
}

#[test]
fn client_stat_error_cases_handled_correctly() {
    let env = TestEnvironment::new();

  
    let missing_path = env.mount_point.join("does-not-exist");
    let result = fs::metadata(&missing_path);

    assert!(
        result.is_err(),
        "La richiesta di metadati per un elemento inesistente doveva fallire"
    );
}