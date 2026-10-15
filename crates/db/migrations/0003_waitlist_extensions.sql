-- Extensions beyond the legacy waitlist schema; the legacy tables are unchanged.

-- The waitlist a registered fleet serves (optional `waitlist_id` of `POST /api/fleet/register`).
-- Goes away with the fleet's registration.
CREATE TABLE `fleet_waitlist` (
  `fleet_id` bigint NOT NULL,
  `waitlist_id` bigint NOT NULL,
  PRIMARY KEY (`fleet_id`),
  KEY `waitlist_id` (`waitlist_id`),
  CONSTRAINT `fleet_waitlist_ibfk_1` FOREIGN KEY (`fleet_id`) REFERENCES `fleet` (`id`) ON DELETE CASCADE,
  CONSTRAINT `fleet_waitlist_ibfk_2` FOREIGN KEY (`waitlist_id`) REFERENCES `waitlist` (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- The last fleet invite sent for a waitlist fit (`POST /api/waitlist/invite`). Goes away with the
-- fit, however it leaves the waitlist. `invited_by` is the inviting account's main; `invited_at`
-- is Unix seconds.
CREATE TABLE `waitlist_invite` (
  `fit_id` bigint NOT NULL,
  `invited_by` bigint NOT NULL,
  `invited_at` bigint NOT NULL,
  PRIMARY KEY (`fit_id`),
  KEY `invited_by` (`invited_by`),
  CONSTRAINT `waitlist_invite_ibfk_1` FOREIGN KEY (`fit_id`) REFERENCES `waitlist_entry_fit` (`id`) ON DELETE CASCADE,
  CONSTRAINT `waitlist_invite_ibfk_2` FOREIGN KEY (`invited_by`) REFERENCES `character` (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
