// Loads build/LoopCraft.driver the way coreaudiod does (CFPlugIn + factory +
// driver interface) and exercises it with a fake host. Nothing is installed.
//   clang -framework CoreFoundation -framework CoreAudio -o build/plugin_harness tools/plugin_harness.c
//   build/plugin_harness build/LoopCraft.driver
#include <CoreAudio/AudioServerPlugIn.h>
#include <CoreFoundation/CoreFoundation.h>
#include <mach/mach_time.h>
#include <math.h>
#include <stdio.h>
#include <string.h>
#include <pthread.h>
#include <unistd.h>

static int failures = 0;
#define CHECK(cond, ...) do { if (cond) { printf("ok   "); } else { printf("FAIL "); failures++; } printf(__VA_ARGS__); printf("\n"); } while (0)

static AudioServerPlugInDriverRef gDriver;
static volatile int gChanged = 0, gConfigRequests = 0;
// Callbacks made while a SetPropertyData call is still running deadlock a real coreaudiod.
static volatile int gInSet = 0, gCallbacksDuringSet = 0;
static volatile int gInInitialize = 0, gCallbacksDuringInitialize = 0;
static pthread_t gSetThread;
#define SYNC_CALLBACK() (gInSet && pthread_equal(pthread_self(), gSetThread))
static CFStringRef gStored = NULL;

static OSStatus host_properties_changed(AudioServerPlugInHostRef h, AudioObjectID o, UInt32 n, const AudioObjectPropertyAddress *a) { if (SYNC_CALLBACK()) gCallbacksDuringSet++; if (gInInitialize) gCallbacksDuringInitialize++; gChanged += n; return 0; }
static OSStatus host_copy(AudioServerPlugInHostRef h, CFStringRef k, CFPropertyListRef *out) {
    *out = gStored ? CFRetain(gStored) : NULL;
    return gStored ? 0 : kAudioHardwareUnspecifiedError;
}
static OSStatus host_write(AudioServerPlugInHostRef h, CFStringRef k, CFPropertyListRef d) {
    if (SYNC_CALLBACK()) gCallbacksDuringSet++;
    if (gStored) CFRelease(gStored);
    gStored = CFRetain(d);
    return 0;
}
static OSStatus host_delete(AudioServerPlugInHostRef h, CFStringRef k) { return 0; }
static OSStatus host_request(AudioServerPlugInHostRef h, AudioObjectID dev, UInt64 action, void *info) {
    if (SYNC_CALLBACK()) gCallbacksDuringSet++;
    gConfigRequests++;
    // A real host stops IO on that device, then asks the driver to apply the change.
    return (*gDriver)->PerformDeviceConfigurationChange(gDriver, dev, action, info);
}
static AudioServerPlugInHostInterface gHost = { host_properties_changed, host_copy, host_write, host_delete, host_request };

static AudioObjectPropertyAddress addr(AudioObjectPropertySelector s, AudioObjectPropertyScope sc) {
    AudioObjectPropertyAddress a = { s, sc, kAudioObjectPropertyElementMain }; return a;
}

static OSStatus get(AudioObjectID o, AudioObjectPropertySelector s, AudioObjectPropertyScope sc, UInt32 size, void *out, UInt32 *outSize) {
    AudioObjectPropertyAddress a = addr(s, sc);
    return (*gDriver)->GetPropertyData(gDriver, o, 0, &a, 0, NULL, size, outSize, out);
}

static void get_string(AudioObjectID o, AudioObjectPropertySelector s, char *buf, size_t n) {
    CFStringRef str = NULL; UInt32 sz;
    buf[0] = 0;
    if (get(o, s, kAudioObjectPropertyScopeGlobal, sizeof(CFStringRef), &str, &sz) == 0 && str) {
        CFStringGetCString(str, buf, (CFIndex)n, kCFStringEncodingUTF8);
        CFRelease(str);
    }
}

static AudioObjectID translate(const char *uid) {
    CFStringRef s = CFStringCreateWithCString(NULL, uid, kCFStringEncodingUTF8);
    AudioObjectPropertyAddress a = addr(kAudioPlugInPropertyTranslateUIDToDevice, kAudioObjectPropertyScopeGlobal);
    AudioObjectID found = 0; UInt32 n;
    (*gDriver)->GetPropertyData(gDriver, kAudioObjectPlugInObject, 0, &a, sizeof(CFStringRef), &s, sizeof found, &n, &found);
    CFRelease(s);
    return found;
}

// Host callbacks arrive asynchronously from the plug-in's callback thread.
static int wait_for(volatile int *counter, int target) {
    for (int i = 0; i < 200 && *counter < target; i++) usleep(5000);
    return *counter >= target;
}

static UInt32 stream_channels(AudioObjectID dev, AudioObjectPropertyScope scope, AudioObjectID *stream) {
    AudioObjectID streams[4]; UInt32 n = 0;
    get(dev, kAudioDevicePropertyStreams, scope, sizeof streams, streams, &n);
    if (n < 4) return 0;
    AudioStreamBasicDescription f; UInt32 sz;
    get(streams[0], kAudioStreamPropertyVirtualFormat, kAudioObjectPropertyScopeGlobal, sizeof f, &f, &sz);
    if (stream) *stream = streams[0];
    return f.mChannelsPerFrame;
}

static OSStatus push_devices(const char *json) {
    CFStringRef s = CFStringCreateWithCString(NULL, json, kCFStringEncodingUTF8);
    AudioObjectPropertyAddress a = addr('LCd#', kAudioObjectPropertyScopeGlobal);
    gSetThread = pthread_self();
    gInSet = 1;
    OSStatus st = (*gDriver)->SetPropertyData(gDriver, kAudioObjectPlugInObject, 0, &a, 0, NULL, sizeof(CFStringRef), &s);
    gInSet = 0;
    CFRelease(s);
    return st;
}

static OSStatus io_client(AudioObjectID dev, AudioObjectID stream, UInt32 client, UInt32 op, UInt32 frames, Float64 tin, Float64 tout, float *buf) {
    AudioServerPlugInIOCycleInfo c; memset(&c, 0, sizeof c);
    c.mNominalIOBufferFrameSize = frames; c.mInputTime.mSampleTime = tin; c.mOutputTime.mSampleTime = tout;
    return (*gDriver)->DoIOOperation(gDriver, dev, stream, client, op, frames, &c, buf, NULL);
}

static OSStatus io(AudioObjectID dev, AudioObjectID stream, UInt32 op, UInt32 frames, Float64 tin, Float64 tout, float *buf) {
    return io_client(dev, stream, 1, op, frames, tin, tout, buf);
}

static void add_client(AudioObjectID dev, UInt32 id, const char *bundle) {
    AudioServerPlugInClientInfo c = { id, getpid(), true, CFStringCreateWithCString(NULL, bundle, kCFStringEncodingUTF8) };
    (*gDriver)->AddDeviceClient(gDriver, dev, &c);
    CFRelease(c.mBundleID);
}

/// The device's level control in `scope`.
static AudioObjectID level_control(AudioObjectID dev, AudioObjectPropertyScope scope) {
    AudioObjectID controls[8]; UInt32 n = 0;
    get(dev, kAudioObjectPropertyControlList, kAudioObjectPropertyScopeGlobal, sizeof controls, controls, &n);
    for (UInt32 i = 0; i < n / sizeof(AudioObjectID); i++) {
        AudioClassID cls = 0; AudioObjectPropertyScope sc = 0; UInt32 sz;
        get(controls[i], kAudioObjectPropertyClass, kAudioObjectPropertyScopeGlobal, 4, &cls, &sz);
        get(controls[i], kAudioControlPropertyScope, kAudioObjectPropertyScopeGlobal, 4, &sc, &sz);
        if (cls == kAudioVolumeControlClassID && sc == scope) return controls[i];
    }
    return 0;
}

int main(int argc, char **argv) {
    const char *path = argc > 1 ? argv[1] : "build/LoopCraft.driver";
    CFURLRef url = CFURLCreateFromFileSystemRepresentation(NULL, (const UInt8 *)path, strlen(path), true);
    CFPlugInRef plugin = CFPlugInCreate(NULL, url);
    CHECK(plugin != NULL, "CFPlugInCreate(%s)", path);
    if (!plugin) return 1;
    CFArrayRef factories = CFPlugInFindFactoriesForPlugInType(kAudioServerPlugInTypeUUID);
    CHECK(factories && CFArrayGetCount(factories) > 0, "factory registered for the AudioServerPlugIn type");
    if (!factories || CFArrayGetCount(factories) == 0) return 1;
    IUnknownVTbl **unknown = CFPlugInInstanceCreate(NULL, CFArrayGetValueAtIndex(factories, 0), kAudioServerPlugInTypeUUID);
    CHECK(unknown != NULL, "factory returned an instance");
    if (!unknown) return 1;
    HRESULT hr = (*unknown)->QueryInterface(unknown, CFUUIDGetUUIDBytes(kAudioServerPlugInDriverInterfaceUUID), (LPVOID *)&gDriver);
    CHECK(hr == S_OK && gDriver, "QueryInterface(driver interface)");
    if (hr != S_OK) return 1;
    gInInitialize = 1;
    CHECK((*gDriver)->Initialize(gDriver, &gHost) == 0, "Initialize (empty storage)");
    usleep(100000); // a late asynchronous callback triggered by Initialize would land here
    gInInitialize = 0;
    CHECK(gCallbacksDuringInitialize == 0, "no host callbacks caused by Initialize (%d)", gCallbacksDuringInitialize);

    // Default device and its engine twin.
    AudioObjectID devices[16]; UInt32 n = 0;
    get(kAudioObjectPlugInObject, kAudioPlugInPropertyDeviceList, kAudioObjectPropertyScopeGlobal, sizeof devices, devices, &n);
    CHECK(n == 8, "default: one device + engine twin");
    const char *defUID = "org.kerpo.loopcraft:8E0F3C52-6B7A-4C1E-9D2F-3A5B7C9E1F20";
    AudioObjectID def = translate(defUID);
    char name[256]; get_string(def, kAudioObjectPropertyName, name, sizeof name);
    CHECK(def != 0 && strcmp(name, "LoopCraft Rack") == 0, "default device keeps the M1 UID and name (\"%s\")", name);
    char twinUID[256]; snprintf(twinUID, sizeof twinUID, "%s#engine", defUID);
    AudioObjectID defTwin = translate(twinUID);
    UInt32 hidden = 0, sz; get(defTwin, kAudioDevicePropertyIsHidden, kAudioObjectPropertyScopeGlobal, 4, &hidden, &sz);
    CHECK(defTwin != 0 && hidden == 1, "engine twin exists and is hidden");

    // Push a device list like the daemon will.
    const char *json = "[{\"uid\":\"org.kerpo.loopcraft:8E0F3C52-6B7A-4C1E-9D2F-3A5B7C9E1F20\",\"name\":\"LoopCraft Rack\"},"
                       "{\"uid\":\"org.kerpo.loopcraft:desktop\",\"name\":\"Desktop\",\"channels\":10,\"pass_thru\":2}]";
    int changedBefore = gChanged;
    CHECK(push_devices(json) == 0, "push device list via custom property");
    CHECK(wait_for(&gChanged, changedBefore + 1), "device-list change notified (asynchronously)");
    for (int i = 0; i < 200 && !gStored; i++) usleep(5000);
    get(kAudioObjectPlugInObject, kAudioPlugInPropertyDeviceList, kAudioObjectPropertyScopeGlobal, sizeof devices, devices, &n);
    CHECK(n == 16, "two devices + two twins");
    CHECK(gStored != NULL, "device list persisted to plug-in storage");
    CHECK(translate("org.kerpo.loopcraft:8E0F3C52-6B7A-4C1E-9D2F-3A5B7C9E1F20") == def, "existing device keeps its object ID");
    AudioObjectID desk = translate("org.kerpo.loopcraft:desktop"), deskTwin = translate("org.kerpo.loopcraft:desktop#engine");
    AudioObjectID deskIn = 0, deskOut = 0, twinIn = 0, twinOut = 0;
    UInt32 inCh = stream_channels(desk, kAudioObjectPropertyScopeInput, &deskIn);
    UInt32 outCh = stream_channels(desk, kAudioObjectPropertyScopeOutput, &deskOut);
    CHECK(inCh == 10 && outCh == 2, "Desktop: 10 output channels to record, 2 Pass-Thru to play into (%u/%u)", inCh, outCh);
    UInt32 tIn = stream_channels(deskTwin, kAudioObjectPropertyScopeInput, &twinIn);
    UInt32 tOut = stream_channels(deskTwin, kAudioObjectPropertyScopeOutput, &twinOut);
    CHECK(tIn == 2 && tOut == 10, "Desktop engine twin: reads 2 Pass-Thru, writes 10 output channels");

    // Live channel-count change through the configuration-change protocol.
    int reqBefore = gConfigRequests;
    push_devices("[{\"uid\":\"org.kerpo.loopcraft:8E0F3C52-6B7A-4C1E-9D2F-3A5B7C9E1F20\",\"name\":\"LoopCraft Rack\"},"
                 "{\"uid\":\"org.kerpo.loopcraft:desktop\",\"name\":\"Desktop\",\"channels\":8,\"pass_thru\":2}]");
    wait_for(&gConfigRequests, reqBefore + 2);
    inCh = stream_channels(desk, kAudioObjectPropertyScopeInput, &deskIn);
    CHECK(gConfigRequests - reqBefore == 2 && inCh == 8, "channels 10 -> 8 applied via config change on device and twin");
    CHECK(translate("org.kerpo.loopcraft:desktop") == desk, "reshaped device keeps its object ID");

    // Audio path with an engine: app -> Pass-Thru -> engine -> output channels -> recorder.
    enum { F = 256 };
    CHECK((*gDriver)->StartIO(gDriver, desk, 1) == 0 && (*gDriver)->StartIO(gDriver, deskTwin, 2) == 0, "StartIO on Desktop and its engine");
    float app[F * 2], pt[F * 2], mixed[F * 8], rec[F * 8];
    for (int i = 0; i < F; i++) { app[2 * i] = 0.25f; app[2 * i + 1] = -0.25f; }
    io(desk, deskOut, kAudioServerPlugInIOOperationWriteMix, F, 0, 1000, app);
    memset(pt, 0, sizeof pt);
    io(deskTwin, twinIn, kAudioServerPlugInIOOperationReadInput, F, 1000, 2000, pt);
    CHECK(memcmp(pt, app, sizeof pt) == 0, "engine reads the app's audio from Pass-Thru");
    memset(mixed, 0, sizeof mixed);
    for (int i = 0; i < F; i++) { mixed[8 * i + 6] = pt[2 * i]; mixed[8 * i + 7] = pt[2 * i + 1]; } // wire 1/2 -> 7/8
    io(deskTwin, twinOut, kAudioServerPlugInIOOperationWriteMix, F, 1000, 3000, mixed);
    memset(rec, 0xff, sizeof rec);
    io(desk, deskIn, kAudioServerPlugInIOOperationReadInput, F, 3000, 4000, rec);
    CHECK(memcmp(rec, mixed, sizeof rec) == 0, "recorder gets the engine's 8 output channels (Pass-Thru on 7/8)");
    (*gDriver)->StopIO(gDriver, deskTwin, 2); (*gDriver)->StopIO(gDriver, desk, 1);

    // Without an engine the default device is a plain pass-through.
    AudioObjectID defIn = 0, defOut = 0;
    stream_channels(def, kAudioObjectPropertyScopeInput, &defIn);
    stream_channels(def, kAudioObjectPropertyScopeOutput, &defOut);
    (*gDriver)->StartIO(gDriver, def, 1);
    float out[F * 2], in[F * 2];
    for (int i = 0; i < F; i++) out[2 * i] = out[2 * i + 1] = 0.5f * sinf(2 * M_PI * 1000 * i / 48000.0f);
    io(def, defOut, kAudioServerPlugInIOOperationWriteMix, F, 0, 20000, out);
    io(def, defIn, kAudioServerPlugInIOOperationReadInput, F, 20000, 21000, in);
    CHECK(memcmp(in, out, sizeof in) == 0, "no engine: played audio comes back unchanged");

    // Clock.
    Float64 zs, zs2; UInt64 zh, zh2, seed, seed2;
    (*gDriver)->GetZeroTimeStamp(gDriver, def, 1, &zs, &zh, &seed);
    usleep(400000);
    (*gDriver)->GetZeroTimeStamp(gDriver, def, 1, &zs2, &zh2, &seed2);
    mach_timebase_info_data_t tb; mach_timebase_info(&tb);
    double measured = (zs2 - zs) / ((zh2 - zh) / (1e9 * tb.denom / tb.numer));
    CHECK(seed == seed2 && zs2 > zs && fabs(measured - 48000) < 1, "clock runs at %.3f Hz", measured);
    (*gDriver)->StopIO(gDriver, def, 1);

    // Gain control: a console-style fader in dB, starting at unity.
    AudioObjectID gainOut = level_control(def, kAudioObjectPropertyScopeOutput);
    AudioValueRange dbRange = {0}; Float32 db = -1, scalar = -1;
    get(gainOut, kAudioLevelControlPropertyDecibelRange, kAudioObjectPropertyScopeGlobal, sizeof dbRange, &dbRange, &sz);
    get(gainOut, kAudioLevelControlPropertyDecibelValue, kAudioObjectPropertyScopeGlobal, 4, &db, &sz);
    get(gainOut, kAudioLevelControlPropertyScalarValue, kAudioObjectPropertyScopeGlobal, 4, &scalar, &sz);
    CHECK(gainOut && dbRange.mMinimum == -96 && dbRange.mMaximum == 12 && db == 0 && scalar == 0.75f,
          "gain control: %.0f..%+.0f dB, starts at %.1f dB (slider %.2f)", dbRange.mMinimum, dbRange.mMaximum, db, scalar);

    // App capture: Spotify gets its own slot and leaves the mix (exclusive).
    reqBefore = gConfigRequests;
    push_devices("[{\"uid\":\"org.kerpo.loopcraft:8E0F3C52-6B7A-4C1E-9D2F-3A5B7C9E1F20\",\"name\":\"LoopCraft Rack\"},"
                 "{\"uid\":\"org.kerpo.loopcraft:desktop\",\"name\":\"Desktop\",\"channels\":8,\"pass_thru\":2,"
                 "\"apps\":[{\"bundle_ids\":[\"com.spotify.client\"],\"exclusive\":true}]}]");
    wait_for(&gConfigRequests, reqBefore + 2);
    char listed[4096]; get_string(kAudioObjectPlugInObject, 'LCd#', listed, sizeof listed);
    CHECK(strstr(listed, "com.spotify.client") != NULL, "device list reports the app slot");
    AudioObjectID twinStreams[4]; UInt32 tsn = 0;
    get(deskTwin, kAudioDevicePropertyStreams, kAudioObjectPropertyScopeInput, sizeof twinStreams, twinStreams, &tsn);
    CHECK(tsn / sizeof(AudioObjectID) == 2, "engine twin: virtual inputs + one app stream (%u)", (unsigned)(tsn / sizeof(AudioObjectID)));
    AudioObjectID appStream = twinStreams[1];
    Boolean willDo = 0, inPlace = 0;
    (*gDriver)->WillDoIOOperation(gDriver, desk, 7, 'pout', &willDo, &inPlace);
    CHECK(willDo && inPlace, "plug-in sees each client's output (ProcessOutput, in place)");
    add_client(desk, 7, "com.spotify.client");
    add_client(desk, 8, "com.apple.Music");
    (*gDriver)->StartIO(gDriver, desk, 7); (*gDriver)->StartIO(gDriver, deskTwin, 2);
    float spotify[F * 2], music[F * 2], mix[F * 2], vin[F * 2], app0[F * 2];
    for (int i = 0; i < F * 2; i++) { spotify[i] = 0.5f; music[i] = 0.25f; }
    io_client(desk, deskOut, 7, 'pout', F, 30000, 31000, spotify);
    io_client(desk, deskOut, 8, 'pout', F, 30000, 31000, music);
    CHECK(spotify[0] == 0 && music[0] == 0.25f, "Spotify is taken out of the mix, Music stays");
    for (int i = 0; i < F * 2; i++) mix[i] = spotify[i] + music[i];
    io(desk, deskOut, kAudioServerPlugInIOOperationWriteMix, F, 30000, 31000, mix);
    io(deskTwin, twinIn, kAudioServerPlugInIOOperationReadInput, F, 31000, 32000, vin);
    io(deskTwin, appStream, kAudioServerPlugInIOOperationReadInput, F, 31000, 32000, app0);
    CHECK(vin[0] == 0.25f && vin[F * 2 - 1] == 0.25f, "engine's virtual inputs carry Music only");
    CHECK(app0[0] == 0.5f && app0[F * 2 - 1] == 0.5f, "engine's app stream carries Spotify");
    AudioServerPlugInClientInfo gone = { 7, getpid(), true, NULL };
    (*gDriver)->RemoveDeviceClient(gDriver, desk, &gone);
    for (int i = 0; i < F * 2; i++) spotify[i] = 0.5f;
    io_client(desk, deskOut, 7, 'pout', F, 30256, 31256, spotify);
    CHECK(spotify[0] == 0.5f, "a removed client is no longer captured");
    (*gDriver)->StopIO(gDriver, deskTwin, 2); (*gDriver)->StopIO(gDriver, desk, 7);

    // Device volume left to the daemon (outputs that follow it): what apps
    // play in stays at unity even with the gain control at -12 dB.
    Float32 minus12 = -12;
    AudioObjectPropertyAddress dbAddr = addr(kAudioLevelControlPropertyDecibelValue, kAudioObjectPropertyScopeGlobal);
    (*gDriver)->SetPropertyData(gDriver, gainOut, 0, &dbAddr, 0, NULL, 4, &minus12);
    float half[F * 2], back[F * 2];
    (*gDriver)->StartIO(gDriver, def, 1);
    for (int i = 0; i < F * 2; i++) half[i] = 0.5f;
    io(def, defOut, kAudioServerPlugInIOOperationWriteMix, F, 0, 40000, half);
    io(def, defIn, kAudioServerPlugInIOOperationReadInput, F, 40000, 41000, back);
    CHECK(fabsf(back[0] - 0.5f * 0.2512f) < 0.001f, "gain control at -12 dB scales what apps play in (%.4f)", back[0]);
    push_devices("[{\"uid\":\"org.kerpo.loopcraft:8E0F3C52-6B7A-4C1E-9D2F-3A5B7C9E1F20\",\"name\":\"LoopCraft Rack\",\"apply_gain\":false},"
                 "{\"uid\":\"org.kerpo.loopcraft:desktop\",\"name\":\"Desktop\",\"channels\":8,\"pass_thru\":2,"
                 "\"apps\":[{\"bundle_ids\":[\"com.spotify.client\"],\"exclusive\":true}]}]");
    for (int i = 0; i < F * 2; i++) half[i] = 0.5f;
    io(def, defOut, kAudioServerPlugInIOOperationWriteMix, F, 0, 42000, half);
    io(def, defIn, kAudioServerPlugInIOOperationReadInput, F, 42000, 43000, back);
    get(gainOut, kAudioLevelControlPropertyDecibelValue, kAudioObjectPropertyScopeGlobal, 4, &db, &sz);
    CHECK(back[0] == 0.5f && db == -12, "apply_gain false: unity in, control still reads %.0f dB", db);
    (*gDriver)->StopIO(gDriver, def, 1);

    // Virtual devices: Comms (playback-only) feeds the engine, the engine
    // feeds Daw Output (record-only); both are devices of their own.
    reqBefore = gConfigRequests;
    push_devices("[{\"uid\":\"org.kerpo.loopcraft:8E0F3C52-6B7A-4C1E-9D2F-3A5B7C9E1F20\",\"name\":\"LoopCraft Rack\"},"
                 "{\"uid\":\"org.kerpo.loopcraft:desktop\",\"name\":\"Desktop\",\"channels\":8,\"pass_thru\":2,"
                 "\"virtuals\":[{\"id\":\"comms\",\"name\":\"Comms\",\"channels\":2},"
                 "{\"id\":\"daw-out\",\"name\":\"Daw Output\",\"channels\":4,\"output\":true}]}]");
    wait_for(&gConfigRequests, reqBefore + 2);
    get(kAudioObjectPlugInObject, kAudioPlugInPropertyDeviceList, kAudioObjectPropertyScopeGlobal, sizeof devices, devices, &n);
    AudioObjectID comms = translate("org.kerpo.loopcraft:desktop/comms"), dawOut = translate("org.kerpo.loopcraft:desktop/daw-out");
    char vname[256]; get_string(comms, kAudioObjectPropertyName, vname, sizeof vname);
    CHECK(n == 24 && comms && dawOut && strcmp(vname, "Desktop: Comms") == 0, "virtual devices listed (%u devices, \"%s\")", (unsigned)(n / sizeof(AudioObjectID)), vname);
    AudioObjectID commsOut = 0, dawIn = 0, none = 0;
    UInt32 commsCh = stream_channels(comms, kAudioObjectPropertyScopeOutput, &commsOut);
    UInt32 commsIn = stream_channels(comms, kAudioObjectPropertyScopeInput, &none);
    UInt32 dawCh = stream_channels(dawOut, kAudioObjectPropertyScopeInput, &dawIn);
    UInt32 dawOutCh = stream_channels(dawOut, kAudioObjectPropertyScopeOutput, &none);
    CHECK(commsCh == 2 && commsIn == 0 && dawCh == 4 && dawOutCh == 0, "Comms plays only (2 ch), Daw Output records only (4 ch)");
    AudioObjectID tins[4], touts[4]; UInt32 ti = 0, to = 0;
    get(deskTwin, kAudioDevicePropertyStreams, kAudioObjectPropertyScopeInput, sizeof tins, tins, &ti);
    get(deskTwin, kAudioDevicePropertyStreams, kAudioObjectPropertyScopeOutput, sizeof touts, touts, &to);
    CHECK(ti / sizeof(AudioObjectID) == 2 && to / sizeof(AudioObjectID) == 2, "engine twin: + Comms input stream, + Daw Output output stream");
    (*gDriver)->StartIO(gDriver, comms, 1); (*gDriver)->StartIO(gDriver, dawOut, 1); (*gDriver)->StartIO(gDriver, deskTwin, 2);
    float discord[F * 2], fromComms[F * 2], toDaw[F * 4], recorded[F * 4];
    for (int i = 0; i < F * 2; i++) discord[i] = 0.375f;
    for (int i = 0; i < F * 4; i++) toDaw[i] = -0.125f;
    io(comms, commsOut, kAudioServerPlugInIOOperationWriteMix, F, 0, 50000, discord);
    io(deskTwin, tins[1], kAudioServerPlugInIOOperationReadInput, F, 50000, 51000, fromComms);
    io(deskTwin, touts[1], kAudioServerPlugInIOOperationWriteMix, F, 50000, 51000, toDaw);
    io(dawOut, dawIn, kAudioServerPlugInIOOperationReadInput, F, 51000, 52000, recorded);
    CHECK(fromComms[0] == 0.375f && fromComms[F * 2 - 1] == 0.375f, "engine reads what is played into Comms");
    CHECK(recorded[0] == -0.125f && recorded[F * 4 - 1] == -0.125f, "Daw Output records what the engine writes");
    UInt32 domain1 = 0, domain2 = 0;
    get(comms, kAudioDevicePropertyClockDomain, kAudioObjectPropertyScopeGlobal, 4, &domain1, &sz);
    get(desk, kAudioDevicePropertyClockDomain, kAudioObjectPropertyScopeGlobal, 4, &domain2, &sz);
    CHECK(domain1 == domain2, "virtual devices share the mixer's clock domain");
    (*gDriver)->StopIO(gDriver, deskTwin, 2); (*gDriver)->StopIO(gDriver, dawOut, 1); (*gDriver)->StopIO(gDriver, comms, 1);

    // Removing a device.
    push_devices("[{\"uid\":\"org.kerpo.loopcraft:8E0F3C52-6B7A-4C1E-9D2F-3A5B7C9E1F20\",\"name\":\"LoopCraft Rack\"}]");
    CHECK(translate("org.kerpo.loopcraft:desktop") == kAudioObjectUnknown, "removed device is gone");
    CHECK(push_devices("[{\"uid\":\"x\",\"name\":\"\"}]") != 0, "invalid device list is rejected");

    CHECK(gCallbacksDuringSet == 0, "no host callbacks from inside SetPropertyData (%d)", gCallbacksDuringSet);
    printf("%d property notifications seen\n", gChanged);
    printf("%s (%d failures)\n", failures ? "FAILED" : "ALL PASSED", failures);
    return failures ? 1 : 0;
}
