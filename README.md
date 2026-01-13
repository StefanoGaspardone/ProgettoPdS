#  ProgettoPdS - Remote Filesystem

##  Descrizione del Progetto
Questo progetto implementa un **filesystem remoto** basato su un'architettura **Client-Server**.

L'obiettivo è permettere di montare una cartella virtuale sul proprio computer locale; le operazioni eseguite su questa cartella (creazione file, scrittura, lettura, cancellazione) vengono intercettate dal **Client** e inviate al **Server** remoto che gestisce l'effettivo storage dei dati.

---

##  Architettura del Codice
Il progetto è diviso in due componenti principali:

###  Server (`/server`)
- Scritto in **Node.js**
- Gestisce le richieste in arrivo dal client
- Mantiene lo stato dei file

###  Client (`/client`)
- Scritto in **Rust**
- Si interfaccia con il kernel del sistema operativo per montare il filesystem
- Usa:
  - **FUSE** (via `fuser`) su Linux/macOS
  - **Dokan** (via `dokany`) su Windows

---

##  Prerequisiti

### Generale (Tutti i sistemi)
- **Node.js** (per il server)
- **Rust & Cargo** (per il client)
- **Nodemon** (opzionale, per il server)
  ```bash
  npm i -g nodemon
  ```

###  Linux
Assicurati di avere installato i pacchetti per FUSE:
```bash
sudo apt install build-essential pkg-config libssl-dev libfuse3-dev libfuse-dev
```

###  Windows
È necessario installare i driver **Dokan Library**.  
Scarica e installa l'ultima versione dal sito ufficiale o dal repository GitHub di Dokan.

###  macOS
È necessario installare **macFUSE**.

---

##  Esecuzione

### 1️ Avvio del Server
Il server deve essere avviato **prima** del client.
```bash
cd server
npm install
npm run dev
```
Il server si metterà in ascolto (es. `http://localhost:3000`).

---

### 2️ Avvio del Client
Apri un nuovo terminale.

 **Nota Importante**  
Prima di avviare il client, assicurati che la cartella di mount sia pulita.

```bash
# Esegui dalla root del progetto
rm -rf client/mnt/remote-fs && mkdir -p client/mnt/remote-fs
```

Esegui il client:
```bash
cd client
cargo run
```

> **Nota:** Il comando `cargo run` scaricherà automaticamente le dipendenze Rust la prima volta.

---

##  Test Manuale (Workflow)

Una volta che il client è in esecuzione e la cartella è montata, puoi testare le funzionalità del filesystem.

### Sequenza di Test (Linux / macOS - Bash)
Esegui i seguenti comandi all'interno della cartella montata (`client/mnt/remote-fs`):

```bash
ls -la                  # Lista file iniziali
mkdir files             # Crea cartella
cd files
echo "AAA" > a.txt      # Scrivi su file
cat a.txt               # Leggi file
echo "BBB" >> a.txt     # Appendi al file
cat a.txt               # Verifica append
rm a.txt                # Rimuovi file
cd ..
rmdir files             # Rimuovi cartella
touch empty.txt         # Crea file vuoto
echo "hello" > old.txt
mv old.txt new.txt      # Rinomina file
cat new.txt
mkdir dir_old
mv dir_old dir_new      # Rinomina cartella
ls -la                  # Verifica finale
```

---

## Equivalenti Comandi per Windows (PowerShell)

| Azione | Comando Linux | Comando PowerShell |
|------|---------------|--------------------|
| Lista file | `ls -la` | `Get-ChildItem -Force` |
| Crea cartella | `mkdir dir` | `mkdir dir` |
| Scrivi (nuovo) | `echo "A" > a.txt` | `"A" > a.txt` |
| Scrivi (append) | `echo "B" >> a.txt` | `Add-Content a.txt "B"` |
| Leggi file | `cat a.txt` | `Get-Content a.txt` |
| File vuoto | `touch f.txt` | `New-Item f.txt` |
| Rimuovi file | `rm a.txt` | `rm a.txt` |
| Rimuovi directory | `rmdir dir` | `rmdir dir` |
| Rinomina/Sposta | `mv old new` | `mv old new` |
| Info file | `stat file` | `Get-Item file` |

---

##  Risoluzione Problemi (Troubleshooting)

### Il filesystem non si smonta correttamente?
Se il programma crasha o viene interrotto forzatamente, la cartella potrebbe rimanere "bloccata".

#### Linux
```bash
fusermount3 -uz client/mnt/remote-fs
# Oppure
sudo umount -l client/mnt/remote-fs
```

#### Windows
Dokan solitamente smonta automaticamente il filesystem alla chiusura dell'applicazione.  
Se il problema persiste, prova a:
- Riavviare il sistema
- Usare il gestore dischi di Dokan
