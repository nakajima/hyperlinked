use super::parse_ps_process_stats;

#[test]
fn parse_ps_process_stats_reads_cpu_and_memory() {
    let stats =
        parse_ps_process_stats(1234, " 1234  12.5  65536\n").expect("ps stats should parse");

    assert_eq!(stats.pid, 1234);
    assert_eq!(stats.cpu_percent, Some(12.5));
    assert_eq!(stats.resident_memory_bytes, Some(64 * 1024 * 1024));
    assert_eq!(stats.error, None);
}

#[test]
fn parse_ps_process_stats_rejects_wrong_pid() {
    let error =
        parse_ps_process_stats(1234, " 9999  1.0  2048\n").expect_err("wrong pid should fail");

    assert!(error.contains("expected 1234"));
}

#[test]
fn parse_ps_process_stats_rejects_empty_output() {
    let error = parse_ps_process_stats(1234, "\n").expect_err("empty output should fail");

    assert!(error.contains("empty"));
}
