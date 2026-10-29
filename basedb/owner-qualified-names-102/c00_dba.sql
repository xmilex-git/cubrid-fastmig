-- fixture C: serial references in trigger text by owner (same owner, other non-DBA owner, DBA)
CREATE USER u1;
CREATE USER u2;
CREATE SERIAL sd START WITH 500;
