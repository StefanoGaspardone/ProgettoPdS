import express from 'express';
import morgan from 'morgan';
import fs from 'fs';
import path from 'path';

const REMOTE_FS_ROOT = './mnt/remote-fs';

const PORT = 3000;
const app = express();

app.use(express.text({ type: '*/*' }));
app.use(morgan('dev'));

const pathExists = async (filePath) => {
    try {
        await fs.promises.access(filePath);
        return true;
    } catch {
        return false;
    }
}

const getPermissionsString = (mode, isDirectory) => {
    const isOwnerRead = (mode & fs.constants.S_IRUSR) !== 0;
    const isOwnerWrite = (mode & fs.constants.S_IWUSR) !== 0;
    const isOwnerExecute = (mode & fs.constants.S_IXUSR) !== 0;

    const isGroupRead = (mode & fs.constants.S_IRGRP) !== 0;
    const isGroupWrite = (mode & fs.constants.S_IWGRP) !== 0;
    const isGroupExecute = (mode & fs.constants.S_IXGRP) !== 0;

    const isOthersRead = (mode & fs.constants.S_IROTH) !== 0;
    const isOthersWrite = (mode & fs.constants.S_IWOTH) !== 0;
    const isOthersExecute = (mode & fs.constants.S_IXOTH) !== 0;

    let perms = isDirectory ? 'd' : '-';
    perms += isOwnerRead ? 'r' : '-';
    perms += isOwnerWrite ? 'w' : '-';
    perms += isOwnerExecute ? 'x' : '-';
    perms += isGroupRead ? 'r' : '-';
    perms += isGroupWrite ? 'w' : '-';
    perms += isGroupExecute ? 'x' : '-';
    perms += isOthersRead ? 'r' : '-';
    perms += isOthersWrite ? 'w' : '-';
    perms += isOthersExecute ? 'x' : '-';

    return perms;
}

/* APIs */

// List directory contents
app.get('/list{/*path}', async (req, res) => {
    try {
        const dirPath = req.params.path ? req.params.path.join('/') : '';
        const fullPath = path.resolve(REMOTE_FS_ROOT, dirPath);
        
        if(!await pathExists(fullPath)) return res.status(404).json({ success: false, message: `No path "${dirPath}" existing` });

        const contents = await fs.promises.readdir(fullPath);
        const detailedContents = await Promise.all(contents.map(async (name) => {
            const namePath = path.join(fullPath, name);
            const stats = await fs.promises.stat(namePath);

            return {
                name,
                type: stats.isDirectory() ? 'dir' : 'file',
                size: stats.size,
                timestamp: stats.mtime,
                permissions: getPermissionsString(stats.mode, stats.isDirectory()),
            };
        }));

        return res.status(200).json({ success: true, contents: detailedContents });
    } catch(error) {
        console.log(error);
        return res.status(500).json({ success: false, message: error.message });
    }
});

app.listen(PORT, () => console.log(`SERVER LISTENING ON http://localhost:${PORT}`));