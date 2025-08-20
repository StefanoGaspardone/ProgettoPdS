use crate::remote_api::{DirEntry, RemoteApi};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use winfsp::ffi::WIN32_FILE_ATTRIBUTE_DIRECTORY;
use winfsp::{
    FileContext, FileInfo, FileSystem, FileSystemContext, IoResult, ReadRequest, Request,
    SecurityDescriptor, WriteRequest,
};

// Mappatura semplice da path a attributi per la cache
lazy_static::lazy_static! {
    static ref ATTR_CACHE: Arc<Mutex<HashMap<PathBuf, DirEntry>>> = Arc::new(Mutex::new(HashMap::new()));
}

#[derive(Clone)]
struct RemoteWinFS {
    api: RemoteApi,
}

impl FileSystem for RemoteWinFS {
    fn get_volume_info(&self, _fs_context: &FileSystemContext, _request: &Request) -> IoResult<winfsp::VolumeInfo> {
        Ok(winfsp::VolumeInfo {
            total_size: 1024 * 1024 * 1024, // 1 GB
            free_size: 1024 * 1024 * 1024,
            volume_label: "RemoteFS".to_string(),
        })
    }

    fn get_file_info(&self, _fs_context: &FileSystemContext, _request: &Request, path: &Path) -> IoResult<FileInfo> {
        let mut cache = ATTR_CACHE.lock().unwrap();
        // Controlla la root
        if path.as_os_str().is_empty() || path == Path::new("/") {
            return Ok(FileInfo::directory(0));
        }

        // Cerca nel parent
        let parent = path.parent().unwrap_or(Path::new("/"));
        let file_name = path.file_name().unwrap().to_str().unwrap();

        match self.api.list_directory(parent) {
            Ok(entries) => {
                if let Some(entry) = entries.iter().find(|e| e.name == file_name) {
                    cache.insert(path.to_path_buf(), entry.clone());
                    let mut file_info = if entry.is_dir {
                        FileInfo::directory(entry.size)
                    } else {
                        FileInfo::file(entry.size)
                    };
                    // I permessi di base in Windows sono attributi
                    file_info.file_attributes = if entry.is_dir { WIN32_FILE_ATTRIBUTE_DIRECTORY } else { 0 };
                    return Ok(file_info);
                }
                Err(winfsp::win_err_to_io_err(winfsp::WinError::ERROR_FILE_NOT_FOUND))
            }
            Err(_) => Err(winfsp::win_err_to_io_err(winfsp::WinError::ERROR_FILE_NOT_FOUND)),
        }
    }

    fn read_directory(
        &self, _fs_context: &FileSystemContext, _request: &Request, path: &Path,
        _marker: Option<&str>, mut readdir: impl FnMut(FileInfo) -> bool,
    ) -> IoResult<()> {
        if let Ok(entries) = self.api.list_directory(path) {
            for entry in entries {
                let mut file_info = if entry.is_dir {
                    FileInfo::directory_with_name(entry.size, &entry.name)
                } else {
                    FileInfo::file_with_name(entry.size, &entry.name)
                };
                file_info.file_attributes = if entry.is_dir { WIN32_FILE_ATTRIBUTE_DIRECTORY } else { 0 };
                
                if !readdir(file_info) {
                    break;
                }
            }
        }
        Ok(())
    }

    fn create(
        &self, _fs_context: &FileSystemContext, _request: &Request, path: &Path,
        _create_options: winfsp::CreateOptions, _granted_access: u32,
    ) -> IoResult<(FileContext, FileInfo)> {
        // Crea un file vuoto se non esiste
        if self.api.read_file(path).is_err() {
           self.api.write_file(path, vec![]).map_err(|_| winfsp::win_err_to_io_err(winfsp::WinError::ERROR_ACCESS_DENIED))?;
        }
        // Crea una directory
        if path.extension().is_none() { // Semplice euristica
             self.api.create_directory(path).map_err(|_| winfsp::win_err_to_io_err(winfsp::WinError::ERROR_ACCESS_DENIED))?;
        }

        let file_info = self.get_file_info(_fs_context, _request, path)?;
        Ok((FileContext::none(), file_info))
    }

    fn read(&self, _fs_context: &FileSystemContext, _request: &Request, path: &Path, request: ReadRequest) -> IoResult<Vec<u8>> {
        let data = self.api.read_file(path).map_err(|_| winfsp::win_err_to_io_err(winfsp::WinError::ERROR_FILE_NOT_FOUND))?;
        let offset = request.offset() as usize;
        let length = request.len() as usize;

        if offset >= data.len() {
            return Ok(Vec::new());
        }

        let end = (offset + length).min(data.len());
        Ok(data[offset..end].to_vec())
    }
    
    fn write(&self, _fs_context: &FileSystemContext, _request: &Request, path: &Path, request: WriteRequest) -> IoResult<u32> {
        // Implementazione semplificata che sovrascrive il file
        let data = request.buffer().to_vec();
        self.api.write_file(path, data).map_err(|_| winfsp::win_err_to_io_err(winfsp::WinError::ERROR_ACCESS_DENIED))?;
        Ok(request.len())
    }

    fn delete(
        &self, _fs_context: &FileSystemContext, _request: &Request, path: &Path,
        _file_context: FileContext,
    ) -> IoResult<()> {
        self.api.delete_path(path).map_err(|_| winfsp::win_err_to_io_err(winfsp::WinError::ERROR_ACCESS_DENIED))
    }

    // Altri metodi come `open`, `close`, `flush`, `set_file_info` potrebbero essere implementati per
    // una maggiore completezza.
}

// Funzione di avvio per WinFSP
pub fn mount(mount_point: &str, remote_url: String) {
    let fs = RemoteWinFS {
        api: RemoteApi::new(remote_url),
    };

    println!("Mounting remote filesystem at {}", mount_point);
    let mut service = winfsp::service::Service::new(mount_point, fs).unwrap();
    println!("Press Ctrl-C to unmount and exit.");
    service.run().unwrap();
}