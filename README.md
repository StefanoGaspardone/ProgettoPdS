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

-----

make sure to delete all files inside client/mnt/remote-fs before running the client

-----
