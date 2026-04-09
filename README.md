Monitor ICC profile will not load under wayland (for now). It uses specific X11 specific APIs

There are some dependencies required to use Vibeprint Studio

UBUNTU
# Core dependencies
sudo apt install \
    libcups2 \
    cups-client \
    libcups2-dev \
    liblcms2-2 \
    liblcms2-dev \
    libx11-6 \
    libx11-dev \
    libxext6 \
    libxext-dev \
    libxrandr2 \
    libxrandr-dev

# For monitor ICC detection (optional feature)
sudo apt install \
    libx11-dev \
    libxrandr-dev

FEDORA
# Core dependencies
sudo dnf install \
    cups-libs \
    cups-client \
    lcms2 \
    lcms2-devel \
    libX11 \
    libX11-devel \
    libXext \
    libXext-devel \
    libXrandr \
    libXrandr-devel

# For monitor ICC detection (optional feature)
sudo dnf install \
    libX11-devel \
    libXrandr-devel

