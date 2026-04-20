import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';

document.getElementById('test-notif')?.addEventListener('click', async () => {
  try {
    await invoke('create_notification_window', { title: "Test", body: "Hello World!" });
  } catch(e) {
    console.error(e);
  }
});

const volumeSlider = document.getElementById('volume-slider');
const volumeVal = document.getElementById('volume-val');

volumeSlider?.addEventListener('input', (e) => {
  if (volumeVal) volumeVal.textContent = e.target.value;
});

document.getElementById('test-audio')?.addEventListener('click', async () => {
  try {
    const vol = parseFloat(volumeSlider?.value || "0.5");
    await invoke('play_notification_sound', { volume: vol });
  } catch(e) {
    console.error(e);
  }
});

document.getElementById('test-upload')?.addEventListener('click', async () => {
  const statusEl = document.getElementById('upload-status');
  try {
    const selected = await open({
      multiple: false,
      directory: false,
    });
    
    if (selected === null) {
      if(statusEl) statusEl.textContent = 'Anulowano wybór pliku.';
      return;
    }
    
    if(statusEl) statusEl.textContent = `Wybrano: ${selected}. Streamowanie...`;
    
    const result = await invoke('upload_file_stream', { filePath: selected });
    
    if(statusEl) statusEl.textContent = `Sukces: ${result}`;
  } catch(e) {
    console.error("Błąd przesyłania:", e);
    if(statusEl) statusEl.textContent = `Błąd: ${e}`;
  }
});

document.getElementById('login-google')?.addEventListener('click', async () => {
  const statusEl = document.getElementById('login-status');
  if(statusEl) statusEl.textContent = "Oczekiwanie na zalogowanie w przeglądarce...";
  
  try {
    const token = await invoke('authenticate_google');
    const truncated = token.length > 20 ? token.substring(0, 20) + "..." : token;
    if(statusEl) statusEl.textContent = `Zalogowano! Token: ${truncated}`;
    
    // Przebudowa Frontendu z autoryzacji na Chat
    const loginCard = document.getElementById('login-card');
    if (loginCard) loginCard.style.display = 'none';
    
    const chatSect = document.getElementById('chat-section');
    if (chatSect) chatSect.style.display = 'block';

  } catch(e) {
    console.error("Błąd logowania:", e);
    if(statusEl) statusEl.textContent = `Błąd autoryzacji: ${e}`;
  }
});

// Podłączanie logiki nowej fazy:
document.getElementById('fetch-spaces')?.addEventListener('click', async () => {
    try {
        const spacesStr = await invoke('fetch_spaces');
        const spacesObj = JSON.parse(spacesStr);
        
        const select = document.getElementById('spaces-list');
        select.innerHTML = '<option value="">Wybierz czat z listy...</option>';
        
        if (spacesObj.spaces && spacesObj.spaces.length > 0) {
            spacesObj.spaces.forEach(space => {
                const opt = document.createElement('option');
                opt.value = space.name;
                opt.textContent = space.displayName || space.name;
                select.appendChild(opt);
            });
            select.disabled = false;
        } else {
            select.innerHTML = '<option value="">Brak wyników (lub pusty profil)</option>';
        }
    } catch(e) {
        console.error("Błąd pobierania:", e);
    }
});

document.getElementById('spaces-list')?.addEventListener('change', (e) => {
    const val = e.target.value;
    const btnPolling = document.getElementById('start-polling');
    const inputMsg = document.getElementById('message-text');
    const btnSend = document.getElementById('send-message');
    
    // Aktywuj zestaw komend
    const isActive = !!val;
    if (btnPolling) btnPolling.disabled = !isActive;
    if (inputMsg) inputMsg.disabled = !isActive;
    if (btnSend) btnSend.disabled = !isActive;
});

document.getElementById('start-polling')?.addEventListener('click', async () => {
    const spaceName = document.getElementById('spaces-list').value;
    const status = document.getElementById('polling-status');
    if (!spaceName) return;
    
    try {
        await invoke('start_message_polling', { spaceName });
        status.textContent = "Poller na rdzeniu Rust uodporniony w tle! Nadaj wiadomość od zewnątrz aby sprawdzić rezonans.";
        document.getElementById('start-polling').disabled = true;
    } catch(e) {
        status.textContent = "Błąd Rust: " + e;
    }
});

document.getElementById('send-message')?.addEventListener('click', async () => {
    const spaceName = document.getElementById('spaces-list').value;
    const text = document.getElementById('message-text').value;
    const status = document.getElementById('send-status');
    
    if (!spaceName || !text) return;
    status.textContent = "Wysyłanie pliku payload po reqwest...";
    
    try {
        await invoke('send_message', { spaceName, text });
        status.textContent = "Zgłoszono odbiór!";
        document.getElementById('message-text').value = '';
    } catch(e) {
        status.textContent = "Błąd HTTP: " + e;
    }
});
