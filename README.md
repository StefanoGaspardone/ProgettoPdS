# ProgettoPdS

SERVER
npm i
npm run dev
(make sure nodemon is installed - npm i -g nodemon)

- FUSE - Linux  
make sure "build-essential", "pkg-config", "libssl-dev", "libfuse3-dev", "libfuse-dev" are installed

  - ls -la
  - mkdir files
  - cd files
  - ls -la
  - echo "AAA" > a.txt
  - cat a.txt
  - echo "BBB" >> a.txt
  - cat a.txt
  - rm a.txt
  - cd ..
  - rmdir files
  - touch empty.txt
  - : > empty.txt
  - stat empty.txt
  - echo "hello" > old.txt
  - mv old.txt new.txt
  - cat new.txt
  - mkdir dir_old
  - mv dir_old dir_new
  - ls -la

  - fusermount3 -uz client/mnt/remote-fs || fusermount -uz client/mnt/remote-fs || sudo umount -l client/mnt/remote-fs
  - rm -rf client/mnt/remote-fs && mkdir -p client/mnt/remote-fs

- DOKAN - Windows  
make sure "Dokan Library" is installed

- macFUSE - MacOS  
make sure "macFUSE" is installed

-----

make sure to delete all files inside client/mnt/remote-fs before running the client

-----

## Equivalenti dei comandi Linux su Windows (PowerShell e cmd)

Di seguito gli equivalenti diretti per i comandi che usi su Linux. Per ognuno mostro la versione per PowerShell (consigliato) e per `cmd` quando differente.

- Cambiare directory:
  - Linux: `cd dir`
  - PowerShell / cmd: `cd dir`

- Lista file (ls -la):
  - Linux: `ls -la`
  - PowerShell: `Get-ChildItem -Force` (alias `ls`, `dir`)
  - cmd: `dir`

- Creare cartelle (mkdir):
  - Linux: `mkdir dir`
  - PowerShell: `New-Item -ItemType Directory -Path dir` oppure `mkdir dir`
  - cmd: `mkdir dir`

- Scrivere su file (sovrascrivere / appendere):
  - Linux: `echo "AAA" > a.txt` / `echo "BBB" >> a.txt`
  - PowerShell: `"AAA" > a.txt` (sovrascrive), `"BBB" >> a.txt` (appende); o `Set-Content a.txt "AAA"` / `Add-Content a.txt "BBB"`
  - cmd: `echo AAA > a.txt` / `echo BBB >> a.txt` TODO

- Visualizzare il contenuto (cat):
  - Linux: `cat a.txt`
  - PowerShell: `Get-Content a.txt`
  - cmd: `type a.txt`

- Creare file vuoto / truncare (touch / `: > file`):
  - Linux: `touch empty.txt` / `: > empty.txt`
  - PowerShell: `New-Item -ItemType File -Force empty.txt` oppure `"" > empty.txt`
  - cmd: `type nul > empty.txt`

- Rimuovere file (rm):
  - Linux: `rm a.txt`
  - PowerShell: `Remove-Item a.txt` (alias `rm`)
  - cmd: `del a.txt`

- Rimuovere directory (vuota o ricorsiva):
  - Linux: `rmdir dir` oppure `rm -r dir`
  - PowerShell: `Remove-Item -Recurse -Force dir`
  - cmd: `rmdir dir`

- Rinominare / spostare (mv):
  - Linux: `mv old.txt new.txt` / `mv dir_old dir_new`
  - PowerShell: `Move-Item old.txt new.txt`
  - cmd: `move old.txt new.txt`

- Copiare file (cp):
  - Linux: `cp src dest`
  - PowerShell: `Copy-Item src dest`
  - cmd: `copy src dest`

- Stat file (stat):
  - Linux: `stat file`
  - PowerShell: `Get-Item file | Format-List *` oppure `Get-ChildItem file | Select-Object *`
  - cmd: non disponibile nativamente (usa PowerShell per info dettagliate)

Se vuoi, converto gli esempi Linux già presenti sopra (con `a.txt`, `old.txt`, `dir_old`, ecc.) in una lista passo-passo equivalente per PowerShell e `cmd`.
