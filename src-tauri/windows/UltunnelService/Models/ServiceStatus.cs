namespace UltunnelService.Models;

public class ServiceStatus {
	public string? LastCommandId { get; set; }
	public bool Running { get; set; }
	public int? Pid { get; set; }
	public string? Message { get; set; }
}