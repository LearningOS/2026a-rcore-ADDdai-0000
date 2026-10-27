# 实验一：多道程序与分时多任务

## 一、实现功能总结

本次实验实现了 sys_trace 系统调用，支持读取和修改当前任务指定地址的一个字节，以及查询当前任务各系统调用的调用次数。
在任务控制块中增加独立的计数数组，并在系统调用分发前更新计数，使统计包含本次调用。
通过实验，我理解了系统调用的处理流程和以及分时多任务环境下按任务维护信息的方法。

## 二、简答题

### 1. 用户态权限限制实验
  本次实验使用 QEMU 运行内核，SBI 实现为 RustSBI，版本为 0.4.1。
  执行命令：
```bash
  make -C os run CHAPTER=3 TEST=2 BASE=1
  ```

三个 bad 测例的结果如下：

1. `ch2b_bad_address` 尝试向地址 `0x0` 写入数据。内核报告
   `PageFault in application, bad addr = 0x0`，随后终止该任务。

2. `ch2b_bad_instructions` 尝试在 U 态执行特权指令 `sret`，
   触发非法指令异常。内核报告 `IllegalInstruction`，随后终止该任务。

3. `ch2b_bad_register` 尝试在 U 态通过 `csrr` 读取 S 态寄存器
   `sstatus`，触发非法指令异常。内核报告 `IllegalInstruction`，
   随后终止该任务。

三个任务被终止后，Hello World 程序仍正常输出，power_3、
power_5、power_7 均输出测试成功信息。说明内核能够处理这些用户
程序异常，并继续调度其他任务。后两个测例验证了 U 态程序不能
直接执行 S 态特权指令或访问 S 态 CSR。

### 2. 进入 __restore 时的栈指针

刚进入 `__restore` 时，`sp` 指向什么？这个函数会在哪两种场景下使用？

答：刚进入 __restore 时，sp 指向当前任务内核栈上待恢复的 TrapContext 的起始地址。
该函数有两种使用场景：一是在用户程序触发 Trap、内核处理完毕后，恢复保存的上下文并返回用户态；
二是在首次启动用户程序时，恢复内核预先构造的初始上下文，通过 sret 进入用户态并从应用入口开始执行。

### 3. 恢复特殊寄存器

分析以下代码：恢复了哪些信息？它们如何影响接下来进入用户态的过程？

```asm
ld t0, 32*8(sp)
ld t1, 33*8(sp)
ld t2, 2*8(sp)
csrw sstatus, t0
csrw sepc, t1
csrw sscratch, t2
```

答：这段代码从 TrapContext 中取出保存的 sstatus、sepc 和用户栈指针。
sstatus 中的 SPP 字段决定执行 sret 后返回的特权级，本章返回用户态时该字段为 U；
sepc 指定返回后开始执行的指令地址；
用户栈指针暂存于 sscratch，稍后通过交换 sp 与 sscratch 恢复，使用户程序能够继续使用自己的栈。

### 4. 通用寄存器恢复中的例外

下面的恢复过程为什么没有处理 `x2` 和 `x4`？分别说明。

```asm
ld x1, 1*8(sp)
ld x3, 3*8(sp)
.set n, 5
.rept 27
    LOAD_GP %n
    .set n, n+1
.endr
```

答：x2 是栈指针 sp。
恢复过程中仍需通过当前的内核栈指针访问 TrapContext，因此不能提前将其恢复为用户栈指针，而是在其他寄存器恢复完毕后，通过交换 sp 与 sscratch 单独恢复。
x4 是线程指针 tp，由于本章应用不使用它，框架省略了它的保存和恢复。

### 5. __restore 中交换栈指针

在 `__restore` 中执行下面这条指令后，`sp` 和 `sscratch` 各保存什么？

```asm
csrrw sp, sscratch, sp
```

答：交换前：sp → 内核栈，sscratch → 用户栈  
交换后：sp → 用户栈，sscratch → 内核栈  
执行该指令后，sp 恢复为用户栈指针，sscratch 保存回收 TrapContext 空间后的内核栈指针。
这样既为返回用户态做好准备，也为下一次 Trap 时切换到内核栈保留了入口。

### 6. 返回用户态

`__restore` 中哪条指令完成特权级切换？为什么执行后会进入 U 态？

答：__restore 最后的 sret 指令完成特权级切换。  
此前恢复的 sstatus 中，SPP 字段为 0，表示返回 U 态。  
因此执行 sret 后，CPU 进入用户态，并从 sepc 保存的地址开始执行。  
*修改栈指针本身不会改变特权级。*

### 7. __alltraps 中交换栈指针

在 `__alltraps` 入口执行下面这条指令后，`sp` 和 `sscratch` 各保存什么？

```asm
csrrw sp, sscratch, sp
```

答：执行该指令后，sp 保存内核栈指针，供内核分配 TrapContext 并执行后续处理；
sscratch 保存 Trap 发生时的用户栈指针，随后会将其写入 TrapContext 的 x[2] 槽位，以便返回用户态时恢复。

### 8. 用户态进入内核态

在本章的系统调用路径中，哪条指令触发从 U 态进入 S 态？

答：用户程序执行 ecall 指令，CPU 切换到 S 态并跳转到 stvec 指定的 __alltraps 入口。

## 三、荣誉准则与参考说明

来源：[课程荣誉准则](https://learningos.cn/rCore-Tutorial-Guide/honorcode.html)。以下保留课程声明原文，并填写本次交流与参考情况。

1. 在完成本次实验的过程（含此前学习的过程）中，我曾分别与 **以下各位** 就（与本次实验相关的）以下方面做过交流，还在代码中对应的位置以注释形式记录了具体的交流对象及内容：

   > 交流与辅助工具为 OpenAI Codex。交流内容包括 Rust 语法、系统调用流程、任务控制块、Trap 上下文、寄存器保存与恢复，以及测试输出和编译错误的解释。

2. 此外，我也参考了 **以下资料**，还在代码中对应的位置以注释形式记录了具体的参考来源及内容：

   > 参考了 [rCore-Tutorial-Guide](https://learningos.cn/rCore-Tutorial-Guide/) 第一至第三章及其示例代码，用于理解运行环境、Trap 处理、任务切换与系统调用；参考了 [第三章练习要求](https://learningos.cn/rCore-Tutorial-Guide/chapter3/5exercise.html)，用于确定 `sys_trace` 的接口、计数规则和测试方式；参考并运行了 [rCore-Tutorial-Test](https://github.com/LearningOS/rCore-Tutorial-Test) 中的相关测例，包括三个 `ch2b_bad_*` 测例。另参考了上述 Codex 对话中提供的实现示例与解释。

3. 我独立完成了本次实验除以上方面之外的所有工作，包括代码与文档。我清楚地知道，从以上方面获得的信息在一定程度上降低了实验难度，可能会影响起评分。

4. 我从未使用过他人的代码，不管是原封不动地复制，还是经过了某些等价转换。
我未曾也不会向他人（含此后各届同学）复制或公开我的实验代码，我有义务妥善保管好它们。
我提交至本实验的评测系统的代码，均无意于破坏或妨碍任何计算机系统的正常运转。
我清楚地知道，以上情况均为本课程纪律所禁止，若违反，对应的实验成绩将按“-100”分计。

## 四、实验反馈（选填）

<!-- 可以记录难点、工作量，以及对实验的建议。 -->

待填写。
