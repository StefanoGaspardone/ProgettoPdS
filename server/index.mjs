import express from 'express';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

// Always resolve relative to this file, not process.cwd()
const REMOTE_FS_ROOT = path.resolve(__dirname, 'mnt/remote-fs');

const PORT = 3000;
const app = express();

app.use(express.json());

const pathExists = async (filePath) => {
    try {
        await fs.promises.access(filePath);
        return true;
    } catch {
        return false;
    }
}

const normalizeRequestPath = (pathParts) => {
    const rawPath = pathParts ? pathParts.join('/') : '';
    const forwardSlashes = rawPath.replaceAll('\\', '/');
    const noLeadingSlashes = forwardSlashes.replace(/^\/+/, '');
    const normalized = path.posix.normalize(noLeadingSlashes);
    const relative = normalized === '.' ? '' : normalized;

    if(relative.startsWith('..') || relative.includes('/../')) throw new Error('Invalid path');

    return relative;
};

const resolveUnderRemoteRoot = (pathParts) => {
    const relativePath = normalizeRequestPath(pathParts);
    const fullPath = path.resolve(REMOTE_FS_ROOT, relativePath);

    if(fullPath !== REMOTE_FS_ROOT && !fullPath.startsWith(REMOTE_FS_ROOT + path.sep)) throw new Error('Invalid path');

    return { relativePath, fullPath };
};

const resolveUnderRemoteRootFromString = (rawPath) => {
    if(typeof rawPath !== 'string') throw new Error('Invalid path');

    const forwardSlashes = rawPath.replaceAll('\\', '/');
    const noLeadingSlashes = forwardSlashes.replace(/^\/+/, '');
    const normalized = path.posix.normalize(noLeadingSlashes);
    const relativePath = normalized === '.' ? '' : normalized;

    if(relativePath.startsWith('..') || relativePath.includes('/../')) throw new Error('Invalid path');

    const fullPath = path.resolve(REMOTE_FS_ROOT, relativePath);
    if(fullPath !== REMOTE_FS_ROOT && !fullPath.startsWith(REMOTE_FS_ROOT + path.sep)) throw new Error('Invalid path');

    return { relativePath, fullPath };
};

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
    console.log('[/list] Richiesta lista contenuti directory');
    try {
        const { relativePath: dirPath, fullPath } = resolveUnderRemoteRoot(req.params.path);
        console.log(`[/list] Path richiesto: ${dirPath}`);
        
        if(!await pathExists(fullPath)) return res.status(404).json({ success: false, message: `Path "${dirPath}" does not exist` });
        if(!(await fs.promises.stat(fullPath)).isDirectory()) return res.status(400).json({ success: false, message: `Path "${dirPath}" does not correspond to a directory` });

        const contents = await fs.promises.readdir(fullPath);
        const detailedContents = await Promise.all(contents.map(async (name) => {
            const namePath = path.resolve(fullPath, name);
            const stats = await fs.promises.stat(namePath);
            const relativePath = path.relative(REMOTE_FS_ROOT, namePath).replaceAll("\\", "/");

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
    console.log('[GET /files] Richiesta lettura contenuto file');
    try {
        const { relativePath: filePath, fullPath } = resolveUnderRemoteRoot(req.params.path);
        console.log(`[GET /files] Path richiesto: ${filePath}`);
        
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
    console.log('[PUT /files] Richiesta scrittura contenuto file');
    try {
        const data = Buffer.isBuffer(req.body) ? req.body : Buffer.alloc(0);
        
        const { relativePath: filePath, fullPath } = resolveUnderRemoteRoot(req.params.path);
        console.log(`[PUT /files] Path richiesto: ${filePath}, dimensione dati: ${data.length} bytes`);
        const offset = Number.isFinite(Number(req.query.offset)) ? Number(req.query.offset) : 0;
        if(offset < 0) return res.status(400).json({ success: false, message: 'Invalid offset' });
        
        if(await pathExists(fullPath) && (await fs.promises.stat(fullPath)).isDirectory()) return res.status(400).json({ success: false, message: `Path "${filePath}" does not correspond to a file` });

        const dirPath = path.dirname(fullPath);
        await fs.promises.mkdir(dirPath, { recursive: true });

        let handle;
        try {
            if(await pathExists(fullPath)) handle = await fs.promises.open(fullPath, 'r+');
            else handle = await fs.promises.open(fullPath, 'w+');

            await handle.write(data, 0, data.length, offset);
        } finally {
            if(handle) await handle.close();
        }
        
        const stats = await fs.promises.stat(fullPath);
        return res.status(201).json({ size: stats.size });
    } catch(error) {
        console.log(error);
        return res.status(500).json({ success: false, message: error.message });
    }
});

// Truncate file to a specific size
app.post('/truncate{/*path}', async (req, res) => {
    console.log('[/truncate] Richiesta troncamento file');
    try {
        const { relativePath: filePath, fullPath } = resolveUnderRemoteRoot(req.params.path);
        const size = req.body?.size;
        console.log(`[/truncate] Path richiesto: ${filePath}, nuova dimensione: ${size}`);

        if(!Number.isInteger(size) || size < 0) return res.status(400).json({ success: false, message: 'Invalid size' });

        if(!await pathExists(fullPath)) return res.status(404).json({ success: false, message: `Path "${filePath}" does not exist` });
        if((await fs.promises.stat(fullPath)).isDirectory()) return res.status(400).json({ success: false, message: `Path "${filePath}" does not correspond to a file` });

        await fs.promises.truncate(fullPath, size);
        return res.status(200).end();
    } catch(error) {
        console.log(error);
        return res.status(500).json({ success: false, message: error.message });
    }
});

// Rename and move a file / dir
app.post('/rename', async (req, res) => {
    console.log('[/rename] Richiesta rinomina/spostamento file o directory');
    try {
        const from = req.body?.from;
        const to = req.body?.to;
        console.log(`[/rename] Da: ${from} -> A: ${to}`);

        const { relativePath: fromPath, fullPath: fromFullPath } = resolveUnderRemoteRootFromString(from);
        const { relativePath: toPath, fullPath: toFullPath } = resolveUnderRemoteRootFromString(to);

        if(fromPath === '' || toPath === '') return res.status(400).json({ success: false, message: 'Invalid path' });
        if(!await pathExists(fromFullPath)) return res.status(404).json({ success: false, message: `Path "${fromPath}" does not exist` });

        await fs.promises.mkdir(path.dirname(toFullPath), { recursive: true });

        await fs.promises.rename(fromFullPath, toFullPath);
        return res.status(200).end();
    } catch(error) {
        console.log(error);
        return res.status(500).json({ success: false, message: error.message });
    }
});

// Create directory
app.post('/mkdir{/*path}', async (req, res) => {
    console.log('[/mkdir] Richiesta creazione directory');
    try {
        const { relativePath: dirPath, fullPath } = resolveUnderRemoteRoot(req.params.path);
        console.log(`[/mkdir] Path richiesto: ${dirPath}`);

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
    console.log('[DELETE /files] Richiesta eliminazione file o directory');
    try {
        const { relativePath: dirPath, fullPath } = resolveUnderRemoteRoot(req.params.path);
        console.log(`[DELETE /files] Path richiesto: ${dirPath}`);
        
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
        const { relativePath: filePath, fullPath } = resolveUnderRemoteRoot(req.params.path);
        
        if(!await pathExists(fullPath)) return res.status(404).json({ success: false, message: `Path "${filePath}" does not exist` });
        
        const stats = await fs.promises.stat(fullPath);
        const isDir = stats.isDirectory();
        
        return res.status(200).json({
            name: path.basename(fullPath),
            path: path.relative(REMOTE_FS_ROOT, fullPath).replaceAll('\\', "/"),
            file_type: isDir ? 'dir' : 'file',
            size: stats.size,
            timestamp: Math.floor(stats.mtimeMs / 1000),
            permissions: getPermissionsString(stats.mode, isDir),
        });
    } catch(error) {
        console.log(error);
        return res.status(500).json({ success: false, message: error.message });
    }
});

/* RUN THE SERVER */
app.listen(PORT, () => console.log(`SERVER LISTENING ON http://localhost:${PORT}`));