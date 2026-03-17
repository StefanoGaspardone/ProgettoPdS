using System.IO;

var builder = WebApplication.CreateBuilder(args);

// Remove limits so you can upload multi-GB files smoothly
builder.WebHost.ConfigureKestrel(options =>
{
    options.Limits.MaxRequestBodySize = null;
});

var app = builder.Build();

var currentDir = Directory.GetCurrentDirectory();
var remoteFsRoot = Path.GetFullPath(Path.Combine(currentDir, "mnt", "remote-fs"));
Directory.CreateDirectory(remoteFsRoot);

// Strictly resolves and prevents directory traversal hacks (like "../../")
string ResolveUnderRoot(string? pathStr)
{
    var cleanPath = pathStr?.TrimStart('/') ?? "";
    cleanPath = cleanPath.Replace('\\', '/');
    
    var fullPath = Path.GetFullPath(Path.Combine(remoteFsRoot, cleanPath));
    
    if (!fullPath.StartsWith(remoteFsRoot))
    {
        throw new BadHttpRequestException("Invalid path escape");
    }
    return fullPath;
}

app.MapGet("/list/{*path}", (string? path, HttpContext ctx) =>
{
    var fullPath = ResolveUnderRoot(path);
    if (!Directory.Exists(fullPath)) return Results.NotFound("Not found");

    var dirInfo = new DirectoryInfo(fullPath);
    var etag = $"W/\"{dirInfo.LastWriteTimeUtc.Ticks}\"";
    
    if (ctx.Request.Headers.IfNoneMatch == etag) return Results.StatusCode(304);

    var entries = dirInfo.GetFileSystemInfos().Select(info => {
        var isDir = info is DirectoryInfo;
        var relPath = string.IsNullOrEmpty(path) ? info.Name : $"{path.TrimEnd('/')}/{info.Name}";
        
        return new FileStat(
            name: info.Name,
            path: relPath.Replace('\\', '/'),
            file_type: isDir ? "dir" : "file",
            size: isDir ? 0 : ((FileInfo)info).Length,
            timestamp: new DateTimeOffset(info.LastWriteTimeUtc).ToUnixTimeSeconds(),
            permissions: isDir ? "rwxr-xr-x" : "rw-r--r--"
        );
    });

    ctx.Response.Headers.ETag = etag;
    ctx.Response.Headers.CacheControl = "public, max-age=1, stale-while-revalidate=5";
    return Results.Ok(entries);
});

app.MapGet("/stat/{*path}", (string? path, HttpContext ctx) =>
{
    var fullPath = ResolveUnderRoot(path);
    
    FileSystemInfo info = Directory.Exists(fullPath) ? new DirectoryInfo(fullPath) : new FileInfo(fullPath);
    if (!info.Exists) return Results.NotFound("Not found");

    var isDir = info is DirectoryInfo;
    var etag = $"W/\"{info.LastWriteTimeUtc.Ticks}\"";
    
    if (ctx.Request.Headers.IfNoneMatch == etag) return Results.StatusCode(304);

    var stat = new FileStat(
        name: info.Name,
        path: (path ?? "").Replace('\\', '/'),
        file_type: isDir ? "dir" : "file",
        size: isDir ? 0 : ((FileInfo)info).Length,
        timestamp: new DateTimeOffset(info.LastWriteTimeUtc).ToUnixTimeSeconds(),
        permissions: isDir ? "rwxr-xr-x" : "rw-r--r--"
    );

    ctx.Response.Headers.ETag = etag;
    ctx.Response.Headers.CacheControl = "public, max-age=1, stale-while-revalidate=5";
    return Results.Ok(stat);
});

app.MapGet("/files/{*path}", async (string? path, long? offset, long? size, HttpContext ctx) =>
{
    var fullPath = ResolveUnderRoot(path);
    var fileInfo = new FileInfo(fullPath);
    
    if (Directory.Exists(fullPath)) return Results.BadRequest("Is a directory");
    if (!fileInfo.Exists) return Results.NotFound("Not found");

    var etag = $"W/\"{fileInfo.Length}-{fileInfo.LastWriteTimeUtc.Ticks}\"";
    if (ctx.Request.Headers.IfNoneMatch == etag) return Results.StatusCode(304);

    var actualOffset = offset ?? 0;
    var actualSize = size ?? Math.Max(0, fileInfo.Length - actualOffset);

    if (actualOffset >= fileInfo.Length) { actualOffset = 0; actualSize = 0; }
    else if (actualOffset + actualSize > fileInfo.Length) { actualSize = fileInfo.Length - actualOffset; }

    ctx.Response.StatusCode = 200;
    ctx.Response.ContentType = "application/octet-stream";
    ctx.Response.ContentLength = actualSize;
    ctx.Response.Headers.ETag = etag;

    // Efficiently stream direct from disk to the network
    using var stream = new FileStream(fullPath, FileMode.Open, FileAccess.Read, FileShare.ReadWrite, 64 * 1024, true);
    stream.Seek(actualOffset, SeekOrigin.Begin);
    
    byte[] buffer = new byte[64 * 1024];
    long remaining = actualSize;
    
    while (remaining > 0)
    {
        // Halts streaming immediately if the user disconnects
        if (ctx.RequestAborted.IsCancellationRequested) break;
        
        int toRead = (int)Math.Min(buffer.Length, remaining);
        int read = await stream.ReadAsync(buffer, 0, toRead, ctx.RequestAborted);
        if (read == 0) break;
        
        await ctx.Response.Body.WriteAsync(buffer, 0, read, ctx.RequestAborted);
        remaining -= read;
    }
    
    return Results.Empty;
});

app.MapPut("/files/{*path}", async (string? path, long? offset, HttpContext ctx) =>
{
    var fullPath = ResolveUnderRoot(path);
    if (Directory.Exists(fullPath)) return Results.BadRequest("Is a directory");

    var parent = Path.GetDirectoryName(fullPath);
    if (parent != null) Directory.CreateDirectory(parent);

    using var stream = new FileStream(fullPath, FileMode.OpenOrCreate, FileAccess.Write, FileShare.None, 64 * 1024, true);
    stream.Seek(offset ?? 0, SeekOrigin.Begin);
    
    // Safely reads the network stream until EOF or client disconnects
    await ctx.Request.Body.CopyToAsync(stream, ctx.RequestAborted);
    
    return Results.Created("", new PutRes(stream.Length));
});

app.MapDelete("/files/{*path}", (string? path) =>
{
    if (string.IsNullOrEmpty(path)) return Results.Conflict("Cannot remove root");
    
    var fullPath = ResolveUnderRoot(path);
    
    if (File.Exists(fullPath))
    {
        File.Delete(fullPath);
    }
    else if (Directory.Exists(fullPath))
    {
        Directory.Delete(fullPath, true);
    }
    else
    {
        return Results.NotFound("Not found");
    }
    
    return Results.Ok();
});

app.MapPost("/mkdir/{*path}", (string? path) =>
{
    var fullPath = ResolveUnderRoot(path);
    
    if (File.Exists(fullPath) || Directory.Exists(fullPath)) return Results.Conflict("Path already exists");
    
    Directory.CreateDirectory(fullPath);
    return Results.Created("", null);
});

app.MapPost("/rename", (RenameReq req) =>
{
    var fromPath = ResolveUnderRoot(req.from);
    var toPath = ResolveUnderRoot(req.to);

    if (!File.Exists(fromPath) && !Directory.Exists(fromPath)) return Results.NotFound("Source not found");

    var parent = Path.GetDirectoryName(toPath);
    if (parent != null) Directory.CreateDirectory(parent);

    if (File.Exists(fromPath)) File.Move(fromPath, toPath);
    else Directory.Move(fromPath, toPath);

    return Results.Ok();
});

app.MapPost("/truncate/{*path}", (string? path, TruncateReq req) =>
{
    var fullPath = ResolveUnderRoot(path);
    
    if (Directory.Exists(fullPath)) return Results.BadRequest("Is a directory");
    if (!File.Exists(fullPath)) return Results.NotFound("Not found");

    using var stream = new FileStream(fullPath, FileMode.Open, FileAccess.Write, FileShare.None);
    stream.SetLength(req.size);
    
    return Results.Ok();
});

Console.WriteLine("C# ASP.NET Core Server listening on http://0.0.0.0:3000");
app.Run("http://0.0.0.0:3000");

// Models matching the Rust JSON structures
record FileStat(string name, string path, string file_type, long size, long timestamp, string permissions);
record PutRes(long size);
record RenameReq(string from, string to);
record TruncateReq(long size);