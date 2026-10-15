-- The wiki (`wl-wiki`, `/api/wiki*`): sections, pages, their revisions, links between pages and
-- uploaded images. Times are Unix seconds; `*_by` / `author_id` are account (main character)
-- ids, NULL for changes made by `waitlist wiki import`. See docs/wiki.md.

-- Groups of the numbered index ("1 Start here", "2 Roles"...). `listed = 0` keeps a section and
-- its pages out of the index (FC cheatsheets, snippets a frontend shows elsewhere).
CREATE TABLE `wiki_section` (
  `key` varchar(64) NOT NULL,
  `label` varchar(255) NOT NULL,
  `position` int NOT NULL,
  `listed` tinyint(1) NOT NULL DEFAULT 1,
  `created_at` bigint NOT NULL,
  `updated_at` bigint NOT NULL,
  PRIMARY KEY (`key`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- A page and its current content (the newest revision, denormalized for reads). `slug` is the
-- page's path (`guides/newbro`); a soft-deleted page keeps it but frees it for new pages
-- (`live_slug` is NULL once `deleted_at` is set). `section_key` has no foreign key so a section
-- can be deleted while deleted pages still name it (restoring such a page picks a new section).
CREATE TABLE `wiki_page` (
  `id` bigint NOT NULL AUTO_INCREMENT,
  `slug` varchar(191) NOT NULL,
  `live_slug` varchar(191) GENERATED ALWAYS AS (IF(`deleted_at` IS NULL, `slug`, NULL)) STORED,
  `section_key` varchar(64) NOT NULL,
  `position` int NOT NULL,
  `title` varchar(255) NOT NULL,
  `summary` text NOT NULL,
  `audience` varchar(255) DEFAULT NULL,
  `body` mediumtext NOT NULL,
  `status` varchar(16) NOT NULL,
  `visibility` varchar(16) NOT NULL,
  `permission` varchar(64) DEFAULT NULL,
  `revision_id` bigint DEFAULT NULL,
  `created_at` bigint NOT NULL,
  `created_by` bigint DEFAULT NULL,
  `updated_at` bigint NOT NULL,
  `updated_by` bigint DEFAULT NULL,
  `published_at` bigint DEFAULT NULL,
  `deleted_at` bigint DEFAULT NULL,
  `deleted_by` bigint DEFAULT NULL,
  PRIMARY KEY (`id`),
  UNIQUE KEY `live_slug` (`live_slug`),
  KEY `section_key` (`section_key`, `position`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Every saved version of a page's content (full text, not deltas).
CREATE TABLE `wiki_revision` (
  `id` bigint NOT NULL AUTO_INCREMENT,
  `page_id` bigint NOT NULL,
  `title` varchar(255) NOT NULL,
  `summary` text NOT NULL,
  `audience` varchar(255) DEFAULT NULL,
  `body` mediumtext NOT NULL,
  `message` varchar(255) NOT NULL DEFAULT '',
  `author_id` bigint DEFAULT NULL,
  `created_at` bigint NOT NULL,
  PRIMARY KEY (`id`),
  KEY `page_id` (`page_id`, `id`),
  CONSTRAINT `wiki_revision_ibfk_1` FOREIGN KEY (`page_id`) REFERENCES `wiki_page` (`id`) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- `/wiki/<slug>` links in the current body of each page (for backlinks).
CREATE TABLE `wiki_link` (
  `page_id` bigint NOT NULL,
  `target` varchar(191) NOT NULL,
  PRIMARY KEY (`page_id`, `target`),
  KEY `target` (`target`),
  CONSTRAINT `wiki_link_ibfk_1` FOREIGN KEY (`page_id`) REFERENCES `wiki_page` (`id`) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Uploaded images. The bytes live in the data directory: `<data>/wiki/assets/<stored_name>`.
CREATE TABLE `wiki_asset` (
  `id` bigint NOT NULL AUTO_INCREMENT,
  `filename` varchar(255) NOT NULL,
  `content_type` varchar(64) NOT NULL,
  `size` bigint NOT NULL,
  `stored_name` varchar(255) NOT NULL,
  `uploaded_by` bigint DEFAULT NULL,
  `uploaded_at` bigint NOT NULL,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Bumped by every wiki write, so the server's in-memory search index notices changes made by
-- other processes (the import command).
CREATE TABLE `wiki_state` (
  `id` tinyint NOT NULL,
  `generation` bigint NOT NULL,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

INSERT INTO `wiki_state` (`id`, `generation`) VALUES (1, 0);
