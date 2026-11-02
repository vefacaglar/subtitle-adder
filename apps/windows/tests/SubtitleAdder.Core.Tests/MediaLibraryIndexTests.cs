using SubtitleAdder.Core;

namespace SubtitleAdder.Core.Tests;

public sealed class MediaLibraryIndexTests : IDisposable
{
    private readonly string _tempRoot;
    private readonly string _indexFilePath;

    public MediaLibraryIndexTests()
    {
        _tempRoot = Path.Combine(Path.GetTempPath(), "SubtitleAdderIndexTests_" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(_tempRoot);
        _indexFilePath = Path.Combine(_tempRoot, "media-index.json");
    }

    public void Dispose()
    {
        try
        {
            if (Directory.Exists(_tempRoot))
            {
                Directory.Delete(_tempRoot, recursive: true);
            }
        }
        catch (IOException)
        {
        }
    }

    [Fact]
    public async Task ScanDirectory_PopulatesMediaFiles_AndDetectsSidecarSubtitles()
    {
        var mediaDir = Path.Combine(_tempRoot, "Videos");
        Directory.CreateDirectory(mediaDir);
        Directory.CreateDirectory(Path.Combine(mediaDir, "SubfolderA"));

        var mp4Path = Path.Combine(mediaDir, "episode01.mp4");
        var srtPath = Path.Combine(mediaDir, "episode01.srt");
        var mkvPath = Path.Combine(mediaDir, "movie.mkv");
        var txtPath = Path.Combine(mediaDir, "notes.txt");

        await File.WriteAllBytesAsync(mp4Path, new byte[2048]);
        await File.WriteAllTextAsync(srtPath, "1\n00:00:01,000 --> 00:00:02,000\nHello\n");
        await File.WriteAllBytesAsync(mkvPath, new byte[4096]);
        await File.WriteAllTextAsync(txtPath, "ignore me");

        var index = new MediaLibraryIndex(_indexFilePath);
        var snapshot = await index.ScanDirectoryAsync(mediaDir);

        Assert.Single(snapshot.Subdirectories);
        Assert.EndsWith("SubfolderA", snapshot.Subdirectories[0], StringComparison.OrdinalIgnoreCase);

        Assert.Equal(2, snapshot.MediaFiles.Count);
        Assert.Equal(2, index.TotalIndexedFiles);

        var ep1 = Assert.Single(snapshot.MediaFiles, f => f.FileName == "episode01.mp4");
        Assert.True(ep1.HasSidecarSubtitle);
        Assert.Equal(".srt", ep1.SidecarExtension);
        Assert.Equal("SRT", ep1.SidecarBadge);
        Assert.Equal(2048, ep1.SizeBytes);

        var movie = Assert.Single(snapshot.MediaFiles, f => f.FileName == "movie.mkv");
        Assert.False(movie.HasSidecarSubtitle);
        Assert.Null(movie.SidecarExtension);
        Assert.Null(movie.SidecarBadge);
        Assert.Equal(4096, movie.SizeBytes);
    }

    [Fact]
    public async Task Persistence_ReloadsCachedEntriesInNewInstance()
    {
        var mediaDir = Path.Combine(_tempRoot, "Library");
        Directory.CreateDirectory(mediaDir);
        var filePath = Path.Combine(mediaDir, "lecture.mp4");
        var vttPath = Path.Combine(mediaDir, "lecture.vtt");
        await File.WriteAllBytesAsync(filePath, new byte[1024]);
        await File.WriteAllTextAsync(vttPath, "WEBVTT\n\n00:00.000 --> 00:01.000\nTest");

        var index1 = new MediaLibraryIndex(_indexFilePath);
        await index1.ScanDirectoryAsync(mediaDir);
        Assert.True(File.Exists(_indexFilePath));

        var index2 = new MediaLibraryIndex(_indexFilePath);
        Assert.Equal(1, index2.TotalIndexedFiles);
        Assert.Single(index2.IndexedDirectories);

        var cached = index2.GetCachedDirectory(mediaDir);
        var item = Assert.Single(cached);
        Assert.Equal("lecture.mp4", item.FileName);
        Assert.True(item.HasSidecarSubtitle);
        Assert.Equal(".vtt", item.SidecarExtension);
    }

    [Fact]
    public async Task RescanningDirectory_PrunesDeletedFiles()
    {
        var mediaDir = Path.Combine(_tempRoot, "PruneTest");
        Directory.CreateDirectory(mediaDir);

        var file1 = Path.Combine(mediaDir, "keep.mp4");
        var file2 = Path.Combine(mediaDir, "remove.mkv");
        await File.WriteAllBytesAsync(file1, [1, 2, 3]);
        await File.WriteAllBytesAsync(file2, [4, 5, 6]);

        var index = new MediaLibraryIndex(_indexFilePath);
        var firstScan = await index.ScanDirectoryAsync(mediaDir);
        Assert.Equal(2, firstScan.MediaFiles.Count);
        Assert.Equal(2, index.TotalIndexedFiles);

        File.Delete(file2);

        var secondScan = await index.ScanDirectoryAsync(mediaDir);
        var remaining = Assert.Single(secondScan.MediaFiles);
        Assert.Equal("keep.mp4", remaining.FileName);
        Assert.Equal(1, index.TotalIndexedFiles);
        Assert.Empty(index.Search("remove"));
    }

    [Fact]
    public async Task CrawlSubtree_AndSearch_FindsFilesAcrossNestedDirectories()
    {
        var rootDir = Path.Combine(_tempRoot, "Root");
        var season1 = Path.Combine(rootDir, "Season1");
        var season2 = Path.Combine(rootDir, "Season2", "Extras");
        Directory.CreateDirectory(season1);
        Directory.CreateDirectory(season2);

        await File.WriteAllBytesAsync(Path.Combine(rootDir, "intro.mp4"), [1, 2]);
        await File.WriteAllBytesAsync(Path.Combine(season1, "s01e01_pilot.mkv"), [1, 2, 3]);
        await File.WriteAllBytesAsync(Path.Combine(season2, "behind_the_scenes.mov"), [1, 2, 3, 4]);

        var index = new MediaLibraryIndex(_indexFilePath);
        await index.CrawlSubtreeAsync(rootDir, maxDepth: 2);

        Assert.Equal(3, index.TotalIndexedFiles);

        var pilotMatches = index.Search("pilot");
        var pilot = Assert.Single(pilotMatches);
        Assert.Equal("s01e01_pilot.mkv", pilot.FileName);

        var behindMatches = index.Search("behind");
        var behind = Assert.Single(behindMatches);
        Assert.Equal("behind_the_scenes.mov", behind.FileName);

        var folderMatches = index.Search("Season1");
        Assert.Single(folderMatches);
        Assert.Equal("s01e01_pilot.mkv", folderMatches[0].FileName);
    }

    [Fact]
    public async Task FileExplorerViewModel_Navigation_Back_Up_AndSearch()
    {
        var parentDir = Path.Combine(_tempRoot, "Parent");
        var childDir = Path.Combine(parentDir, "Child");
        var siblingDir = Path.Combine(_tempRoot, "OtherIndexed");
        Directory.CreateDirectory(parentDir);
        Directory.CreateDirectory(childDir);
        Directory.CreateDirectory(siblingDir);

        await File.WriteAllBytesAsync(Path.Combine(parentDir, "parent_clip.mp4"), new byte[512]);
        await File.WriteAllBytesAsync(Path.Combine(childDir, "child_movie.mkv"), new byte[1024]);
        await File.WriteAllBytesAsync(Path.Combine(siblingDir, "global_special_doc.mp4"), new byte[2048]);

        var index = new MediaLibraryIndex(_indexFilePath);
        await index.ScanDirectoryAsync(siblingDir);

        var settings = new InMemorySettingsStore();
        var vm = new FileExplorerViewModel(index, settings, ImmediateDispatcher.Instance);

        await vm.NavigateToAsync(parentDir, crawlSubfolders: true);
        Assert.Equal(MediaLibraryIndex.NormalizeDirectoryPath(parentDir), vm.CurrentFolder);
        Assert.Equal("Parent", vm.CurrentFolderName);
        Assert.False(vm.CanGoBack);
        Assert.True(vm.CanGoUp);
        Assert.Single(vm.Folders);
        Assert.Equal("Child", vm.Folders[0].Name);
        Assert.Single(vm.Files);
        Assert.Equal("parent_clip.mp4", vm.Files[0].FileName);
        Assert.Equal(3, vm.TotalIndexedFiles);
        Assert.Equal(MediaLibraryIndex.NormalizeDirectoryPath(parentDir), settings.GetString(FileExplorerViewModel.LastFolderDefaultsKey));

        // Navigate into Child
        await vm.NavigateToAsync(childDir, crawlSubfolders: false);
        Assert.Equal("Child", vm.CurrentFolderName);
        Assert.True(vm.CanGoBack);
        Assert.Single(vm.Files);
        Assert.Equal("child_movie.mkv", vm.Files[0].FileName);

        // GoBack returns to Parent
        await vm.GoBackAsync();
        Assert.Equal("Parent", vm.CurrentFolderName);
        Assert.False(vm.CanGoBack);
        Assert.Single(vm.Files);
        Assert.Equal("parent_clip.mp4", vm.Files[0].FileName);

        // Navigate into Child again, then GoUp to Parent
        await vm.NavigateToAsync(childDir, crawlSubfolders: false);
        await vm.GoUpAsync();
        Assert.Equal("Parent", vm.CurrentFolderName);

        // SearchQuery filters current folder AND includes SSD index matches from other folders
        vm.SearchQuery = "global_special";
        var searchResult = Assert.Single(vm.Files);
        Assert.Equal("global_special_doc.mp4", searchResult.FileName);

        // Clearing SearchQuery restores current folder files
        vm.SearchQuery = "";
        Assert.Single(vm.Files);
        Assert.Equal("parent_clip.mp4", vm.Files[0].FileName);
    }

    [Fact]
    public async Task FileExplorerViewModel_SyncToVideoPath_UpdatesSidecarStatus()
    {
        var dir = Path.Combine(_tempRoot, "SyncFolder");
        Directory.CreateDirectory(dir);
        var videoPath = Path.Combine(dir, "interview.mp4");
        await File.WriteAllBytesAsync(videoPath, new byte[1024]);

        var index = new MediaLibraryIndex(_indexFilePath);
        var settings = new InMemorySettingsStore();
        var vm = new FileExplorerViewModel(index, settings, ImmediateDispatcher.Instance);

        await vm.SyncToVideoPathAsync(videoPath);
        Assert.Equal(MediaLibraryIndex.NormalizeDirectoryPath(dir), vm.CurrentFolder);
        var fileBefore = Assert.Single(vm.Files);
        Assert.False(fileBefore.HasSidecarSubtitle);

        await File.WriteAllTextAsync(Path.Combine(dir, "interview.srt"), "1\n00:00:01,000 --> 00:00:02,000\nSub\n");
        await vm.SyncToVideoPathAsync(videoPath);

        var fileAfter = Assert.Single(vm.Files);
        Assert.True(fileAfter.HasSidecarSubtitle);
        Assert.Equal(".srt", fileAfter.SidecarExtension);
    }
}
