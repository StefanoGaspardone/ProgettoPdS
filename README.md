# ProgettoPdS - Remote Filesystem (NFS User Space)

## Descrizione del Progetto

Questo progetto implementa un **Network Filesystem (NFS) in User Space**.  
L'applicazione permette di montare una cartella virtuale sul computer locale (**Client**) che, invece di scrivere su disco, comunica le operazioni a un **Server remoto** tramite protocollo **HTTP**.

Il sistema intercetta le chiamate di sistema (*syscall*) del kernel tramite:
- **FUSE** su Linux/macOS
- **Dokan** su Windows  

e le traduce in richieste **REST API** verso un server **Node.js**.

---

## Architettura Tecnica

Il progetto è strutturato in due componenti principali.

### Server (`/server`) — Node.js
Il server agisce da interfaccia verso lo storage fisico.

- **Protocollo REST**  
  Espone endpoint HTTP per ogni operazione del filesystem  
  *(es. `GET /list` per `ls`, `PUT /files` per la scrittura dati)*.

- **Sicurezza (Path Sanitization)**  
  Implementa controlli rigorosi sui percorsi tramite la funzione  
  `resolveUnderRemoteRoot`, prevenendo attacchi di **Path Traversal**  
  e garantendo che i client non possano accedere a file esterni alla root dedicata.

- **Gestione I/O**  
  Utilizza `fs.promises` per operazioni asincrone ed efficienti su disco.

---

### Client (`/client`) — Rust
Il client è responsabile del montaggio del filesystem e della traduzione delle operazioni.

- **FUSE & Dokan**  
  Usa le librerie:
  - `fuser` (Linux / macOS)
  - `dokany` (Windows)

- **Inode Cache**  
  Il kernel identifica i file tramite **inode**, mentre il server utilizza percorsi stringa.  
  Il client mantiene una `HashMap` in memoria (protetta da `Mutex`) per la traduzione:
  ```
  Inode <-> Path
  ```

- **Ponte Sincrono / Asincrono**  
  Le callback del filesystem sono sincrone, mentre le richieste HTTP sono asincrone.  
  Il client utilizza il runtime **Tokio** per eseguire chiamate `reqwest` all'interno
  delle operazioni del driver filesystem.

---

## Prerequisiti

### Generale
- **Node.js** (v16+ raccomandato)
- **Rust & Cargo** (ultima versione stabile)
- **Nodemon** (opzionale):
  ```bash
  npm i -g nodemon
  ```

### Linux (Debian / Ubuntu)
Installare le librerie necessarie per FUSE:
```bash
sudo apt update
sudo apt install build-essential pkg-config libssl-dev libfuse3-dev libfuse-dev
```

### Windows
Installare **Dokan Library** (driver filesystem).  
Scaricare `DokanSetup.exe` dalle release ufficiali GitHub di Dokan.

### macOS
Installare **macFUSE**.

---

## Esecuzione

### Avvio del Server
Il server deve essere avviato per primo.  
Si metterà in ascolto sulla porta **3000**.

```bash
cd server
npm install
npm run dev
```

---

### Avvio del Client

⚠️ **Nota Importante**  
Prima di avviare il client, è consigliabile pulire la cartella di mount.

```bash
# Dalla root del progetto
rm -rf client/mnt/remote-fs && mkdir -p client/mnt/remote-fs
```

Avvio del client:
```bash
cd client
cargo run
```

> Al primo avvio, Cargo scaricherà e compilerà tutte le dipendenze Rust.

---

## Guida ai Test

Una volta montato il filesystem, la cartella  
`client/mnt/remote-fs` può essere usata come una normale unità di memoria.

### Workflow di Test (Linux / macOS - Bash)

```bash
cd client/mnt/remote-fs

ls -la                  # 1. Lista directory vuota
mkdir files             # 2. Creazione cartella
cd files
echo "AAA" > a.txt      # 3. Scrittura file
cat a.txt               # 4. Lettura
echo "BBB" >> a.txt     # 5. Append
cat a.txt               # 6. Verifica contenuto
rm a.txt                # 7. Cancellazione file
cd ..
rmdir files             # 8. Cancellazione cartella
touch empty.txt         # 9. Creazione file vuoto
stat empty.txt          # 10. Verifica metadati
echo "hello" > old.txt
mv old.txt new.txt      # 11. Rinomina file
cat new.txt
mkdir dir_old
mv dir_old dir_new      # 12. Rinomina directory
ls -la
```

---

## Comandi Equivalenti per Windows

| Azione | Linux | PowerShell | CMD |
|------|------|------------|-----|
| Lista file | `ls -la` | `ls` | `dir` |
| Cambia dir | `cd dir` | `cd dir` | `cd dir` |
| Crea dir | `mkdir dir` | `mkdir dir` | `mkdir dir` |
| Scrivi (nuovo) | `echo A > f.txt` | `"A" > f.txt` | `echo A > f.txt` |
| Scrivi (append) | `echo B >> f.txt` | `Add-Content f.txt "B"` | `echo B >> f.txt` |
| Leggi file | `cat f.txt` | `Get-Content f.txt` | `type f.txt` |
| File vuoto | `touch f.txt` | `New-Item f.txt` | `type nul > f.txt` |
| Rimuovi file | `rm f.txt` | `rm f.txt` | `del f.txt` |
| Rimuovi dir | `rmdir dir` | `rmdir dir` | `rmdir dir` |
| Rinomina | `mv old new` | `mv old new` | `move old new` |

---

## Risoluzione Problemi (Troubleshooting)

Se il programma viene interrotto bruscamente, la cartella di mount potrebbe rimanere bloccata.

### Linux
```bash
fusermount3 -uz client/mnt/remote-fs
# Oppure
sudo umount -l client/mnt/remote-fs
```

### Windows
Il driver **Dokan** gestisce solitamente lo smontaggio automatico.  
Se il drive rimane bloccato:
- Riavviare il sistema
- Usare il tool **Dokan Library Mounter**
