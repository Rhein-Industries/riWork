import { mount } from "nsh";
import { ChatTab } from "../chat/ChatTab";

// chat_image_mount.rs writes this chat before starting its disposable host.
mount(() => <ChatTab chatId="11111111-2222-4333-8444-555555555555" visible={true} />);
