-- Add migration script here
alter table message_queue alter column chats set not null;
alter table message_queue alter column message set not null;
alter table message_queue alter column images set not null;
alter table message_queue alter column datetime set not null;