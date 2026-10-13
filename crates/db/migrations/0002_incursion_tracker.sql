-- Public incursion tracker: spawns seen on ESI, their systems and roles, influence samples,
-- state changes and the tracker's own polling state. Times are Unix seconds (UTC), like
-- `incursion_focus`. The legacy `incursion_focus` table is unchanged and still maintained.

CREATE TABLE `incursion_spawn` (
  `id` bigint NOT NULL AUTO_INCREMENT,
  `constellation_id` bigint NOT NULL,
  `constellation_name` varchar(100) NOT NULL,
  `region_id` bigint DEFAULT NULL,
  `region_name` varchar(100) DEFAULT NULL,
  `faction_id` bigint NOT NULL,
  `incursion_type` varchar(64) NOT NULL,
  `staging_system_id` bigint NOT NULL,
  `staging_system_name` varchar(100) NOT NULL,
  `staging_security` double NOT NULL,
  -- highsec / lowsec / nullsec, by the staging system's displayed security status
  `security_band` varchar(16) NOT NULL,
  -- mobilizing / established / withdrawing (the last state seen while active)
  `state` varchar(16) NOT NULL,
  `influence` double NOT NULL,
  `has_boss` tinyint NOT NULL DEFAULT '0',
  `first_seen_at` bigint NOT NULL,
  -- 1 when the tracker was running when the spawn appeared (first_seen_at is its spawn time)
  `observed_from_start` tinyint NOT NULL DEFAULT '0',
  `last_seen_at` bigint NOT NULL,
  `state_changed_at` bigint NOT NULL,
  `mobilizing_at` bigint DEFAULT NULL,
  `withdrawing_at` bigint DEFAULT NULL,
  `boss_spawned_at` bigint DEFAULT NULL,
  -- NULL while active
  `ended_at` bigint DEFAULT NULL,
  PRIMARY KEY (`id`),
  KEY `incursion_spawn_ended_at` (`ended_at`),
  KEY `incursion_spawn_band_ended_at` (`security_band`, `ended_at`),
  KEY `incursion_spawn_constellation` (`constellation_id`, `ended_at`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE `incursion_spawn_system` (
  `spawn_id` bigint NOT NULL,
  `system_id` bigint NOT NULL,
  `system_name` varchar(100) NOT NULL,
  `security` double NOT NULL,
  -- staging / vanguard / assault / headquarters / unknown
  `role` varchar(16) NOT NULL,
  -- known (published layout) / estimated (derived from the stargate graph) / unknown
  `role_source` varchar(16) NOT NULL,
  PRIMARY KEY (`spawn_id`, `system_id`),
  CONSTRAINT `incursion_spawn_system_spawn` FOREIGN KEY (`spawn_id`) REFERENCES `incursion_spawn` (`id`) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE `incursion_influence` (
  `spawn_id` bigint NOT NULL,
  -- ESI's Last-Modified time of the sample (one sample per ESI refresh, every 5 minutes)
  `sampled_at` bigint NOT NULL,
  `influence` double NOT NULL,
  PRIMARY KEY (`spawn_id`, `sampled_at`),
  CONSTRAINT `incursion_influence_spawn` FOREIGN KEY (`spawn_id`) REFERENCES `incursion_spawn` (`id`) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE `incursion_event` (
  `id` bigint NOT NULL AUTO_INCREMENT,
  `spawn_id` bigint NOT NULL,
  `at` bigint NOT NULL,
  -- spawned / state_changed / boss_spawned / boss_gone / staging_changed / ended
  `kind` varchar(32) NOT NULL,
  `from_state` varchar(16) DEFAULT NULL,
  `to_state` varchar(16) DEFAULT NULL,
  PRIMARY KEY (`id`),
  KEY `incursion_event_at` (`at`),
  KEY `incursion_event_spawn` (`spawn_id`, `at`),
  CONSTRAINT `incursion_event_spawn` FOREIGN KEY (`spawn_id`) REFERENCES `incursion_spawn` (`id`) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE `incursion_tracker_state` (
  `id` tinyint NOT NULL,
  `last_poll_at` bigint NOT NULL,
  `data_modified_at` bigint DEFAULT NULL,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
