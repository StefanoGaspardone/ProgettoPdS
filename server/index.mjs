import express from 'express';
import morgan from 'morgan';
import fs from 'fs';
import path from 'path';

const REMOTE_FS_ROOT = './mnt/remote-fs';

const PORT = 3000;
const app = express();

app.use(express.json());
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

    let perms = isOwnerRead ? 'r' : '-';
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
        
        if(!await pathExists(fullPath)) return res.status(404).json({ success: false, message: `Path "${dirPath}" does not exist` });
        if(!(await fs.promises.stat(fullPath)).isDirectory()) return res.status(400).json({ success: false, message: `Path "${dirPath}" does not correspond to a directory` });

        const contents = await fs.promises.readdir(fullPath);
        const detailedContents = await Promise.all(contents.map(async (name) => {
            const namePath = path.resolve(fullPath, name);
            const stats = await fs.promises.stat(namePath);
            const relativePath = path.relative(REMOTE_FS_ROOT, namePath).replace("\\", "/");

            return {
                name,
                path: relativePath, 
                file_type: stats.isDirectory() ? 'dir' : 'file',
                size: stats.size,
                timestamp: Math.floor(new Date(stats.mtime).getTime() / 1000),
                permissions: getPermissionsString(stats.mode, stats.isDirectory()),
            }
        }));

        return res.status(200).json(detailedContents);
    } catch(error) {
        console.log(error);
        return res.status(500).json({ success: false, message: error.message });
    }
});

// Read file contents
app.get('/files{/*path}', async (req, res) => {
    try {
        const filePath = req.params.path ? req.params.path.join('/') : '';
        const fullPath = path.resolve(REMOTE_FS_ROOT, filePath);
        
        if(!await pathExists(fullPath)) return res.status(404).json({ success: false, message: `Path "${filePath}" does not exist` });
        if((await fs.promises.stat(fullPath)).isDirectory()) return res.status(400).json({ success: false, message: `Path "${filePath}" does not correspond to a file` });

        const content = await fs.promises.readFile(fullPath);
        res.set('Content-Type', 'application/octet-stream');
        return res.status(200).send(content);
    } catch(error) {
        console.log(error);
        return res.status(500).json({ success: false, message: error.message });
    }
});

// Write file contents
app.put('/files{/*path}', express.raw({ type: '*/*', limit: '50mb' }), async (req, res) => {
    try {
        const data = req.body;
        
        const filePath = req.params.path ? req.params.path.join('/') : '';
        const fullPath = path.resolve(REMOTE_FS_ROOT, filePath);
        
        if(await pathExists(fullPath) && (await fs.promises.stat(fullPath)).isDirectory()) return res.status(400).json({ success: false, message: `Path "${filePath}" does not correspond to a file` });

        const dirPath = path.dirname(fullPath);
        await fs.promises.mkdir(dirPath, { recursive: true });

        await fs.promises.writeFile(fullPath, data);
        
        const stats = await fs.promises.stat(fullPath);
        return res.status(201).json({ size: stats.size });
    } catch(error) {
        console.log(error);
        return res.status(500).json({ success: false, message: error.message });
    }
});

// Create directory
app.post('/mkdir{/*path}', async (req, res) => {
    try {
        const dirPath = req.params.path ? req.params.path.join('/') : '';
        const fullPath = path.resolve(REMOTE_FS_ROOT, dirPath);

        if(await pathExists(fullPath)) return res.status(409).json({ success: false, message: `Path "${dirPath}" already exists` });
    
        await fs.promises.mkdir(fullPath, { recursive: true });
        return res.status(201).end();
    } catch(error) {
        console.log(error);
        return res.status(500).json({ success: false, message: error.message });
    }
});

// Delete file or repository
app.delete('/files{/*path}', async (req, res) => {
    try {
        const dirPath = req.params.path ? req.params.path.join('/') : '';
        const fullPath = path.resolve(REMOTE_FS_ROOT, dirPath);
        
        if(dirPath === '') return res.status(409).json({ success: false, message: 'You cannot remove the whole file system' });
        if(!await pathExists(fullPath)) return res.status(404).json({ success: false, message: `Path "${dirPath}" does not exist` });
    
        await fs.promises.rm(fullPath, { recursive: true });
        return res.status(200).end();
    } catch(error) {
        console.log(error);
        return res.status(500).json({ success: false, message: error.message });
    }
});

// Stat file or directory (per FUSE getattr/lookup)
app.get('/stat{/*path}', async (req, res) => {
    try {
        const filePath = req.params.path ? req.params.path.join('/') : '';
        const fullPath = path.resolve(REMOTE_FS_ROOT, filePath);
        
        if (!await pathExists(fullPath)) return res.status(404).json({ success: false, message: `Path "${filePath}" does not exist` });
        
        const stats = await fs.promises.stat(fullPath);
        const isDir = stats.isDirectory();
        
        return res.status(200).json({
            name: path.basename(fullPath),
            path: path.relative(REMOTE_FS_ROOT, fullPath).replace(/\\/g, "/"),
            file_type: isDir ? 'dir' : 'file',
            size: stats.size,
            timestamp: Math.floor(stats.mtimeMs / 1000),
            permissions: getPermissionsString(stats.mode, isDir),
        });
    } catch (error) {
        console.log(error);
        return res.status(500).json({ success: false, message: error.message });
    }
});

/* RUN THE SERVER */
app.listen(PORT, () => console.log(`SERVER LISTENING ON http://localhost:${PORT}`));