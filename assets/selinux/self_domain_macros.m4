#####################################
# injected_domain_self_perms(domain)
# Grant self permissions that are normally inherited from the domain
# attribute at compile time but are missing for runtime-injected types.
define(`injected_domain_self_perms', `
  allow $1 { hwbinder_device binder_device }:chr_file rw_file_perms;
  allow $1 hwservicemanager:hwservice_manager list;

  allow $1 $1:process { fork sigchld sigkill sigstop signull signal getsched setsched getsession getpgid getcap setcap getattr setrlimit };
  allow $1 $1:fd use;
  r_dir_file($1, $1)
  allow $1 $1:{ fifo_file file } rw_file_perms;
  allow $1 $1:unix_stream_socket { create_stream_socket_perms connectto };
  allow $1 $1:unix_dgram_socket { create_socket_perms sendto };

  allow crash_dump $1:process { ptrace sigchld sigkill signal sigstop };
  can_profile_heap($1)
  can_profile_perf($1)
  allow netutils_wrapper $1:process sigchld;
  r_dir_file(uprobestats, $1)

  allow servicemanager $1:binder transfer;
  allow vndservicemanager $1:binder transfer;

  get_prop($1, vendor_default_prop)
  get_prop($1, servicemanager_prop)
  # Public readable properties
  get_prop($1, aaudio_config_prop)
  get_prop($1, apexd_select_prop)
  get_prop($1, arm64_memtag_prop)
  get_prop($1, bluetooth_config_prop)
  get_prop($1, bootloader_prop)
  get_prop($1, build_odm_prop)
  get_prop($1, build_prop)
  get_prop($1, build_vendor_prop)
  get_prop($1, debug_prop)
  get_prop($1, exported_config_prop)
  get_prop($1, exported_default_prop)
  get_prop($1, exported_dumpstate_prop)
  get_prop($1, exported_secure_prop)
  get_prop($1, exported_system_prop)
  get_prop($1, fingerprint_prop)
  get_prop($1, framework_status_prop)
  get_prop($1, gwp_asan_prop)
  get_prop($1, hal_instrumentation_prop)
  get_prop($1, hw_timeout_multiplier_prop)
  get_prop($1, init_service_status_prop)
  get_prop($1, libc_debug_prop)
  get_prop($1, locale_prop)
  get_prop($1, logd_prop)
  get_prop($1, mediadrm_config_prop)
  get_prop($1, property_service_version_prop)
  get_prop($1, soc_prop)
  get_prop($1, socket_hook_prop)
  get_prop($1, surfaceflinger_prop)
  get_prop($1, telephony_status_prop)
  get_prop($1, timezone_prop)
  get_prop($1, userdebug_or_eng_prop)
  get_prop($1, vendor_socket_hook_prop)
  get_prop($1, vndk_prop)
  get_prop($1, vold_status_prop)
  get_prop($1, vts_config_prop)

  # Allow calls to system(3), popen(3), ...
  allow $1 {shell_exec toolbox_exec}:file rx_file_perms;

  allow $1 system_data_root_file:dir { search getattr } ;
  allow $1 system_data_file:dir search;
  allow $1 vendor_userdir_file:dir { getattr search };
  allow $1 vendor_data_file:dir { getattr search };

  allow $1 proc:lnk_file { getattr read };

  allow $1 proc_cpuinfo:file r_file_perms;

  allow $1 proc_perf:file r_file_perms;

  # Filesystem access.
  allow $1 fs_type:filesystem getattr;
  allow $1 fs_type:dir getattr;

  # read APEX dir and stat any symlink pointing to APEXs.
  allow $1 apex_mnt_dir:dir { getattr search };
  allow $1 apex_mnt_dir:lnk_file r_file_perms;

  allow $1 vndk_sp_file:dir r_dir_perms;
  allow $1 vndk_sp_file:file { execute read open getattr map };

  allow $1 vendor_file:dir { getattr search };

  allow $1 vendor_file_type:file rx_file_perms;
  allow $1 vendor_file_type:lnk_file { getattr open read };
  allow $1 vendor_file_type:dir r_dir_perms;
  allow $1 vendor_file_type:file { read open getattr execute map };
  allow $1 vendor_file_type:lnk_file { getattr read };

  allow $1 vendor_configs_file:dir r_dir_perms;
  allow $1 vendor_configs_file:file { read open getattr map };

  allow $1 vendor_hal_file:dir r_dir_perms;

  r_dir_file($1, vendor_file_type)

  allow $1 sysfs:lnk_file { getattr read };

  allow $1 same_process_hal_file:dir r_dir_perms;
  allow $1 same_process_hal_file:file { execute read open getattr map };

  r_dir_file($1, sysfs_devices_system_cpu)
  r_dir_file($1, sysfs_usb);

  # Path resolution access in cgroups.
  allow $1 cgroup:dir search;
  allow $1 cgroup:dir w_dir_perms;
  allow $1 cgroup:file w_file_perms;

  allow $1 cgroup_v2:dir search;
  allow $1 cgroup_v2:dir w_dir_perms;
  allow $1 cgroup_v2:file w_file_perms;

  allow $1 cgroup_desc_file:file r_file_perms;
  allow $1 cgroup_rc_file:dir search;
  allow $1 cgroup_rc_file:file r_file_perms;
  allow $1 task_profiles_file:file r_file_perms;
  allow $1 vendor_cgroup_desc_file:file r_file_perms;
  allow $1 vendor_task_profiles_file:file r_file_perms;
  allow $1 libprocessgroup_metadata_file:dir r_dir_perms;
  allow $1 libprocessgroup_metadata_file:file r_file_perms;
')
