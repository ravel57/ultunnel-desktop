using System.Diagnostics;
using System.ServiceProcess;
using System.Text.Json;
using System.Timers;
using UltunnelService.Models;

namespace UltunnelService;

public class UltunnelWindowsService : ServiceBase {
	private readonly string _baseDir;
	private readonly string _commandFile;
	private readonly string _statusFile;
	private readonly string _logFile;
	private readonly string _singBoxPath;
	private readonly string _configPath;

	private string? _lastProcessedCommandId;

	private System.Timers.Timer? _timer;
	private Process? _singBoxProcess;
	private bool _tickInProgress;

	private ServiceStatus _currentStatus = new ServiceStatus {
		Running = false,
		Message = "initialized",
		Pid = null
	};

	private readonly JsonSerializerOptions _jsonOptions = new JsonSerializerOptions {
		WriteIndented = true,
		PropertyNameCaseInsensitive = true
	};

	public UltunnelWindowsService() {
		ServiceName = "UltunnelService";
		CanStop = true;
		AutoLog = true;

		_baseDir = Path.Combine(
			Environment.GetFolderPath(Environment.SpecialFolder.CommonApplicationData),
			"Ultunnel"
		);

		_commandFile = Path.Combine(_baseDir, "command.json");
		_statusFile = Path.Combine(_baseDir, "status.json");
		_logFile = Path.Combine(_baseDir, "service.log");
		_singBoxPath = Path.Combine(_baseDir, "sing-box.exe");
		_configPath = Path.Combine(_baseDir, "singbox.json");
	}

	protected override void OnStart(string[] args) {
		try {
			Directory.CreateDirectory(_baseDir);

			Log("Service OnStart");
			Log("BaseDir: " + _baseDir);
			Log("CommandFile: " + _commandFile);
			Log("StatusFile: " + _statusFile);
			Log("SingBoxPath: " + _singBoxPath);
			Log("ConfigPath: " + _configPath);

			_currentStatus = new ServiceStatus {
				Running = IsSingBoxRunning(),
				Message = "Service started",
				Pid = GetSingBoxPid()
			};

			WriteStatus(_currentStatus);

			_timer = new System.Timers.Timer(1000);
			_timer.Elapsed += OnTick;
			_timer.AutoReset = true;
			_timer.Start();

			Log("Service started");
		}
		catch (Exception ex) {
			Log("OnStart error: " + ex);
			throw;
		}
	}

	protected override void OnStop() {
		try {
			Log("Service OnStop");

			_timer?.Stop();
			_timer?.Dispose();
			_timer = null;

			StopSingBox();

			WriteStatus(new ServiceStatus {
				Running = false,
				Message = "Service stopped",
				Pid = null
			});

			Log("Service stopped");
		}
		catch (Exception ex) {
			Log("OnStop error: " + ex);
			throw;
		}
	}

	private void OnTick(object? sender, ElapsedEventArgs e) {
		if (_tickInProgress) {
			return;
		}

		_tickInProgress = true;

		try {
			HandleCommand();

			_currentStatus.Running = IsSingBoxRunning();
			_currentStatus.Pid = GetSingBoxPid();

			if (string.IsNullOrWhiteSpace(_currentStatus.Message)) {
				_currentStatus.Message = "OK";
			}

			WriteStatus(_currentStatus);
		}
		catch (Exception ex) {
			Log("Tick error: " + ex);

			_currentStatus.Running = IsSingBoxRunning();
			_currentStatus.Pid = GetSingBoxPid();
			_currentStatus.Message = ex.Message;

			WriteStatus(_currentStatus);
		}
		finally {
			_tickInProgress = false;
		}
	}

	private void HandleCommand() {
		if (!File.Exists(_commandFile)) {
			return;
		}

		string json;

		try {
			json = File.ReadAllText(_commandFile);
		}
		catch (Exception ex) {
			Log("Cannot read command file: " + ex.Message);
			return;
		}

		if (string.IsNullOrWhiteSpace(json)) {
			return;
		}

		ServiceCommand? cmd;

		try {
			cmd = JsonSerializer.Deserialize<ServiceCommand>(json, _jsonOptions);
		}
		catch (Exception ex) {
			Log("Cannot deserialize command file: " + ex.Message);
			return;
		}

		if (cmd == null || string.IsNullOrWhiteSpace(cmd.Id) || string.IsNullOrWhiteSpace(cmd.Cmd)) {
			Log("Invalid command file. Json: " + json);
			return;
		}

		if (cmd.Id == _lastProcessedCommandId) {
			return;
		}

		_lastProcessedCommandId = cmd.Id;

		string command = cmd.Cmd.Trim().ToLowerInvariant();

		Log($"Processing command: {command}, id={cmd.Id}");

		switch (command) {
			case "start":
				StartSingBox(cmd.ConfigPath);
				_currentStatus = new ServiceStatus {
					LastCommandId = cmd.Id,
					Running = IsSingBoxRunning(),
					Pid = GetSingBoxPid(),
					Message = "started"
				};
				WriteStatus(_currentStatus);
				break;

			case "stop":
				StopSingBox();
				_currentStatus = new ServiceStatus {
					LastCommandId = cmd.Id,
					Running = false,
					Pid = null,
					Message = "stopped"
				};
				WriteStatus(_currentStatus);
				break;

			case "restart":
				StopSingBox();
				StartSingBox(cmd.ConfigPath);
				_currentStatus = new ServiceStatus {
					LastCommandId = cmd.Id,
					Running = IsSingBoxRunning(),
					Pid = GetSingBoxPid(),
					Message = "restarted"
				};
				WriteStatus(_currentStatus);
				break;

			default:
				Log("Unknown command: " + cmd.Cmd);
				_currentStatus = new ServiceStatus {
					LastCommandId = cmd.Id,
					Running = IsSingBoxRunning(),
					Pid = GetSingBoxPid(),
					Message = "Unknown command: " + cmd.Cmd
				};
				WriteStatus(_currentStatus);
				break;
		}
	}

	private void StartSingBox(string? cmdConfigPath) {
		if (IsSingBoxRunning()) {
			Log("sing-box is already running");
			return;
		}

		if (!File.Exists(_singBoxPath)) {
			throw new FileNotFoundException("sing-box.exe not found", _singBoxPath);
		}

		string effectiveConfigPath = !string.IsNullOrWhiteSpace(cmdConfigPath)
			? cmdConfigPath
			: _configPath;

		if (!File.Exists(effectiveConfigPath)) {
			throw new FileNotFoundException("singbox config not found", effectiveConfigPath);
		}

		var psi = new ProcessStartInfo {
			FileName = _singBoxPath,
			Arguments = $"run -c \"{effectiveConfigPath}\"",
			WorkingDirectory = _baseDir,
			UseShellExecute = false,
			RedirectStandardOutput = true,
			RedirectStandardError = true,
			CreateNoWindow = true
		};

		_singBoxProcess = new Process {
			StartInfo = psi,
			EnableRaisingEvents = true
		};

		_singBoxProcess.OutputDataReceived += (_, e) => {
			if (!string.IsNullOrWhiteSpace(e.Data)) {
				Log("[sing-box stdout] " + e.Data);
			}
		};

		_singBoxProcess.ErrorDataReceived += (_, e) => {
			if (!string.IsNullOrWhiteSpace(e.Data)) {
				Log("[sing-box stderr] " + e.Data);
			}
		};

		_singBoxProcess.Exited += (_, _) => {
			try {
				Log("sing-box exited. ExitCode=" + _singBoxProcess?.ExitCode);
			}
			catch {
				Log("sing-box exited");
			}
		};

		if (!_singBoxProcess.Start()) {
			throw new InvalidOperationException("Failed to start sing-box process");
		}

		_singBoxProcess.BeginOutputReadLine();
		_singBoxProcess.BeginErrorReadLine();

		Log("sing-box started, pid=" + _singBoxProcess.Id);
	}

	private void StopSingBox() {
		try {
			if (_singBoxProcess != null && !_singBoxProcess.HasExited) {
				Log("Stopping tracked sing-box process, pid=" + _singBoxProcess.Id);

				_singBoxProcess.Kill(true);
				_singBoxProcess.WaitForExit(5000);
			}
		}
		catch (Exception ex) {
			Log("Stop tracked process error: " + ex);
		}

		_singBoxProcess = null;

		foreach (var p in Process.GetProcessesByName("sing-box")) {
			try {
				Log("Killing sing-box process, pid=" + p.Id);
				p.Kill(true);
				p.WaitForExit(5000);
			}
			catch (Exception ex) {
				Log("Kill sing-box process error, pid=" + p.Id + ", error=" + ex.Message);
			}
		}

		Log("sing-box stopped");
	}

	private bool IsSingBoxRunning() {
		try {
			if (_singBoxProcess != null && !_singBoxProcess.HasExited) {
				return true;
			}

			return Process.GetProcessesByName("sing-box").Length > 0;
		}
		catch (Exception ex) {
			Log("IsSingBoxRunning error: " + ex.Message);
			return false;
		}
	}

	private int? GetSingBoxPid() {
		try {
			if (_singBoxProcess != null && !_singBoxProcess.HasExited) {
				return _singBoxProcess.Id;
			}

			var process = Process.GetProcessesByName("sing-box").FirstOrDefault();
			return process?.Id;
		}
		catch {
			return null;
		}
	}

	private void WriteStatus(ServiceStatus status) {
		try {
			Directory.CreateDirectory(_baseDir);

			string json = JsonSerializer.Serialize(status, _jsonOptions);
			File.WriteAllText(_statusFile, json);
		}
		catch (Exception ex) {
			Log("WriteStatus error: " + ex);
		}
	}

	private void Log(string message) {
		try {
			Directory.CreateDirectory(_baseDir);

			File.AppendAllText(
				_logFile,
				$"[{DateTime.Now:yyyy-MM-dd HH:mm:ss}] {message}{Environment.NewLine}"
			);
		}
		catch {
			// Нельзя ронять службу из-за ошибки записи лога.
		}
	}
}