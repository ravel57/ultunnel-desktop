using System.Diagnostics;
using System.Security.Principal;
using System.ServiceProcess;
using System.Text.Json;
using UltunnelService;

internal static class Program {
	private const string ServiceName = "UltunnelService";

	private static readonly string BaseDir = Path.Combine(
		Environment.GetFolderPath(Environment.SpecialFolder.CommonApplicationData),
		"Ultunnel"
	);

	private static readonly string InstallerLogFile = Path.Combine(BaseDir, "installer.log");
	private static readonly string CommandFile = Path.Combine(BaseDir, "command.json");
	private static readonly string StatusFile = Path.Combine(BaseDir, "status.json");

	static int Main(string[] args) {
		Directory.CreateDirectory(BaseDir);

		string mode = args.FirstOrDefault()?.ToLowerInvariant() ?? "";

		try {
			LogInstaller("Program started. Args: " + string.Join(" ", args));

			switch (mode) {
				case "--service":
					LogInstaller("Starting in Windows Service mode");
					ServiceBase.Run(new UltunnelWindowsService());
					return 0;

				case "--install":
					RequireAdministrator();
					InstallService();
					return 0;

				case "--uninstall":
					RequireAdministrator();
					UninstallService();
					return 0;

				case "--start":
					SendCommand("start");
					return 0;

				case "--stop":
					SendCommand("stop");
					return 0;

				case "--restart":
					SendCommand("restart");
					return 0;

				case "--status":
					PrintStatus();
					return 0;

				default:
					if (!IsAdministrator()) {
						LogInstaller("Not administrator. Relaunching self as admin with --install");
						RelaunchSelfAsAdmin("--install");
						return 0;
					}
					RequireAdministrator();
					InstallService();
					return 0;
			}
		}
		catch (Exception ex) {
			LogInstaller("Fatal error: " + ex);
			Console.Error.WriteLine(ex);
			return 1;
		}
	}

	private static void InstallService() {
		LogInstaller("InstallService started");

		Directory.CreateDirectory(BaseDir);

		string currentDir = AppContext.BaseDirectory;
		string currentExe = Environment.ProcessPath
			?? throw new InvalidOperationException("Cannot get current exe path");

		string targetExe = Path.Combine(BaseDir, Path.GetFileName(currentExe));

		LogInstaller("Current dir: " + currentDir);
		LogInstaller("Current exe: " + currentExe);
		LogInstaller("Install dir: " + BaseDir);
		LogInstaller("Target exe: " + targetExe);

		if (!SameDirectory(currentDir, BaseDir)) {
			CopyDirectory(currentDir, BaseDir);
			LogInstaller("Files copied to install directory");
		}
		else {
			LogInstaller("Current directory is already install directory. Copy skipped");
		}

		if (!File.Exists(targetExe)) {
			throw new FileNotFoundException("Target service exe was not found after copy", targetExe);
		}

		if (ServiceExists(ServiceName)) {
			LogInstaller("Service already exists. Stopping and deleting old service");
			StopServiceIfRunning();
			DeleteService();
			WaitUntilServiceDeleted(ServiceName, TimeSpan.FromSeconds(10));
		}

		string binPath = $"\"{targetExe}\" --service";

		RunSc($"create {ServiceName} binPath= \"{binPath}\" start= auto");
		RunSc($"description {ServiceName} \"Ultunnel background service\"");

		LogInstaller("Starting service");
		RunSc($"start {ServiceName}", ignoreExitCode: true);

		Console.WriteLine("Service installed");
		LogInstaller("Service installed");
	}

	private static void UninstallService() {
		LogInstaller("UninstallService started");

		if (!ServiceExists(ServiceName)) {
			LogInstaller("Service does not exist. Nothing to uninstall");
			Console.WriteLine("Service not found");
			return;
		}

		StopServiceIfRunning();
		DeleteService();

		Console.WriteLine("Service removed");
		LogInstaller("Service removed");
	}

	private static void SendCommand(string cmd) {
		Directory.CreateDirectory(BaseDir);

		var payload = new {
			Id = Guid.NewGuid().ToString("N"),
			Cmd = cmd,
			Timestamp = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds()
		};

		string json = JsonSerializer.Serialize(payload, new JsonSerializerOptions {
			WriteIndented = true
		});

		File.WriteAllText(CommandFile, json);

		Console.WriteLine($"Command sent: {cmd}");
		LogInstaller($"Command sent: {cmd}, file: {CommandFile}");
	}

	private static void PrintStatus() {
		if (!File.Exists(StatusFile)) {
			Console.WriteLine("status.json not found");
			LogInstaller("status.json not found: " + StatusFile);
			return;
		}

		Console.WriteLine(File.ReadAllText(StatusFile));
	}

	private static void RequireAdministrator() {
		if (IsAdministrator()) {
			return;
		}

		throw new InvalidOperationException("Administrator rights required");
	}

	private static bool IsAdministrator() {
		using var identity = WindowsIdentity.GetCurrent();
		var principal = new WindowsPrincipal(identity);
		return principal.IsInRole(WindowsBuiltInRole.Administrator);
	}

	private static void RelaunchSelfAsAdmin(string arguments) {
		var psi = new ProcessStartInfo {
			FileName = Environment.ProcessPath!,
			Arguments = arguments,
			UseShellExecute = true,
			Verb = "runas"
		};

		Process.Start(psi);
	}

	private static bool ServiceExists(string serviceName) {
		var result = RunProcess("sc.exe", $"query {serviceName}", ignoreExitCode: true);
		return result.ExitCode == 0;
	}

	private static void StopServiceIfRunning() {
		LogInstaller("Stopping service if running");
		RunSc($"stop {ServiceName}", ignoreExitCode: true);
		Thread.Sleep(2000);
	}

	private static void DeleteService() {
		LogInstaller("Deleting service");
		RunSc($"delete {ServiceName}", ignoreExitCode: true);
		Thread.Sleep(1500);
	}

	private static void WaitUntilServiceDeleted(string serviceName, TimeSpan timeout) {
		DateTime deadline = DateTime.UtcNow.Add(timeout);

		while (DateTime.UtcNow < deadline) {
			if (!ServiceExists(serviceName)) {
				LogInstaller("Service deleted");
				return;
			}

			Thread.Sleep(500);
		}

		LogInstaller("Service still exists after delete timeout");
	}

	private static void RunSc(string arguments, bool ignoreExitCode = false) {
		var result = RunProcess("sc.exe", arguments, ignoreExitCode);

		if (!ignoreExitCode && result.ExitCode != 0) {
			throw new InvalidOperationException(
				$"sc.exe {arguments} failed\nOUT:\n{result.Stdout}\nERR:\n{result.Stderr}"
			);
		}
	}

	private static ProcessResult RunProcess(string fileName, string arguments, bool ignoreExitCode = false) {
		LogInstaller($"Run process: {fileName} {arguments}");

		var psi = new ProcessStartInfo {
			FileName = fileName,
			Arguments = arguments,
			RedirectStandardOutput = true,
			RedirectStandardError = true,
			UseShellExecute = false,
			CreateNoWindow = true
		};

		using var process = Process.Start(psi)!;

		string stdout = process.StandardOutput.ReadToEnd();
		string stderr = process.StandardError.ReadToEnd();

		process.WaitForExit();

		LogInstaller($"Exit code: {process.ExitCode}");
		if (!string.IsNullOrWhiteSpace(stdout)) {
			LogInstaller("STDOUT: " + stdout.Trim());
		}
		if (!string.IsNullOrWhiteSpace(stderr)) {
			LogInstaller("STDERR: " + stderr.Trim());
		}

		return new ProcessResult(process.ExitCode, stdout, stderr);
	}

	private static void CopyDirectory(string sourceDir, string destinationDir, bool recursive = true) {
		var dir = new DirectoryInfo(sourceDir);

		if (!dir.Exists) {
			throw new DirectoryNotFoundException($"Исходная папка не найдена: {sourceDir}");
		}

		Directory.CreateDirectory(destinationDir);

		foreach (FileInfo file in dir.GetFiles()) {
			string targetFilePath = Path.Combine(destinationDir, file.Name);

			if (Path.GetFullPath(file.FullName).Equals(Path.GetFullPath(targetFilePath), StringComparison.OrdinalIgnoreCase)) {
				continue;
			}

			file.CopyTo(targetFilePath, true);
			LogInstaller($"Copied file: {file.FullName} -> {targetFilePath}");
		}

		if (!recursive) {
			return;
		}

		foreach (DirectoryInfo subDir in dir.GetDirectories()) {
			string newDestinationDir = Path.Combine(destinationDir, subDir.Name);

			if (Path.GetFullPath(subDir.FullName).Equals(Path.GetFullPath(newDestinationDir), StringComparison.OrdinalIgnoreCase)) {
				continue;
			}

			CopyDirectory(subDir.FullName, newDestinationDir);
		}
	}

	private static bool SameDirectory(string a, string b) {
		string fullA = Path.GetFullPath(a).TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
		string fullB = Path.GetFullPath(b).TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);

		return string.Equals(fullA, fullB, StringComparison.OrdinalIgnoreCase);
	}

	private static void LogInstaller(string message) {
		try {
			Directory.CreateDirectory(BaseDir);

			File.AppendAllText(
				InstallerLogFile,
				$"[{DateTime.Now:yyyy-MM-dd HH:mm:ss}] {message}{Environment.NewLine}"
			);
		}
		catch {
			// Нельзя падать из-за ошибки логирования.
		}
	}

	private sealed record ProcessResult(int ExitCode, string Stdout, string Stderr);
}